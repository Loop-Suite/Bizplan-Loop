use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Accumulated cost (in micro-dollars). Sums total_cost_usd from claude's responses.
static COST_MICROS: AtomicU64 = AtomicU64::new(0);

pub fn total_cost_usd() -> f64 {
    COST_MICROS.load(Ordering::Relaxed) as f64 / 1_000_000.0
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    /// Validated structured output when --json-schema is used
    pub structured: Option<serde_json::Value>,
}

/// Backend that invokes the Claude Code CLI (`claude -p`) as a subprocess.
///
/// Applies the following by default (all verified against `claude --help`):
/// - `--safe-mode`      : disables CLAUDE.md / skills / plugins / hooks / MCP (auth still works normally)
/// - `--tools ""`       : disables all built-in tools → pure text generation, no file access
/// - `--no-session-persistence` : no session file created (avoids contention on parallel runs)
/// - `--output-format json`     : collects result / structured_output / total_cost_usd
///
/// `--bare` is not used. It does not read OAuth/keychain, which breaks auth for subscription-login users.
#[derive(Clone, Debug)]
pub struct Llm {
    pub bin: String,
    pub model: Option<String>,
    pub retries: u32,
    pub verbose: bool,
    pub timeout: Duration,
    pub max_budget_usd: Option<f64>,
    /// If true, omits --safe-mode and loads CLAUDE.md etc. from the execution directory
    pub load_context: bool,
}

impl Llm {
    pub fn new(bin: String, model: Option<String>) -> Self {
        Llm {
            bin,
            model,
            retries: 2,
            verbose: false,
            timeout: Duration::from_secs(600),
            max_budget_usd: None,
            load_context: false,
        }
    }

    pub fn label(&self) -> String {
        self.model.clone().unwrap_or_else(|| "default".to_string())
    }

    fn call_once(&self, prompt: &str, system: Option<&str>, schema: Option<&str>) -> Result<Reply> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("-p").arg("--output-format").arg("json");
        if !self.load_context {
            cmd.arg("--safe-mode");
        }
        cmd.arg("--no-session-persistence");
        cmd.arg("--tools").arg("");
        if let Some(m) = &self.model {
            cmd.arg("--model").arg(m);
        }
        if let Some(b) = self.max_budget_usd {
            cmd.arg("--max-budget-usd").arg(format!("{b}"));
        }
        if let Some(s) = system {
            cmd.arg("--append-system-prompt").arg(s);
        }
        if let Some(js) = schema {
            cmd.arg("--json-schema").arg(js);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to run `{}` (check installation and PATH)", self.bin)
        })?;

        // Writing stdin and reading stdout/stderr must happen concurrently to avoid deadlock from pipe buffer saturation.
        let mut sin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to open stdin"))?;
        let payload = prompt.to_string();
        let t_in = std::thread::spawn(move || {
            let _ = sin.write_all(payload.as_bytes());
            // drop(sin) → EOF
        });
        let mut sout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to open stdout"))?;
        let t_out = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = sout.read_to_string(&mut s);
            s
        });
        let mut serr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to open stderr"))?;
        let t_err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = serr.read_to_string(&mut s);
            s
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait()? {
                Some(st) => break st,
                None => {
                    if started.elapsed() > self.timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(anyhow!(
                            "Timeout exceeded {} seconds",
                            self.timeout.as_secs()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        };
        let _ = t_in.join();
        let stdout = t_out.join().unwrap_or_default();
        let stderr = t_err.join().unwrap_or_default();

        if !status.success() {
            return Err(anyhow!(
                "claude exited with code {:?}: {}",
                status.code(),
                truncate(stderr.trim(), 300)
            ));
        }

        let v: serde_json::Value = serde_json::from_str(stdout.trim()).with_context(|| {
            format!(
                "Failed to parse claude JSON output: {}",
                truncate(&stdout, 300)
            )
        })?;
        if v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
            return Err(anyhow!(
                "claude error response (subtype={}): {}",
                v.get("subtype").and_then(|s| s.as_str()).unwrap_or("?"),
                truncate(v.get("result").and_then(|r| r.as_str()).unwrap_or(""), 300)
            ));
        }
        let cost = v
            .get("total_cost_usd")
            .and_then(|c| c.as_f64())
            .unwrap_or(0.0);
        if cost > 0.0 {
            COST_MICROS.fetch_add((cost * 1_000_000.0) as u64, Ordering::Relaxed);
        }
        let text = v
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string();
        let structured = v.get("structured_output").cloned().filter(|s| !s.is_null());
        if text.trim().is_empty() && structured.is_none() {
            return Err(anyhow!("Empty response"));
        }
        Ok(Reply { text, structured })
    }

    fn with_retry<T>(&self, what: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..=self.retries {
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if self.verbose {
                        eprintln!(
                            "[{} retry {}/{}] {}",
                            what,
                            attempt + 1,
                            self.retries,
                            truncate(&format!("{e:#}"), 200)
                        );
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("Unknown failure")))
    }

    /// Generate text.
    pub fn text(&self, prompt: &str, system: Option<&str>) -> Result<String> {
        let r = self.with_retry("generate", || self.call_once(prompt, system, None))?;
        Ok(r.text)
    }

    /// Structured generation enforced with a JSON Schema. Schema validation is performed by the claude CLI,
    /// and the result comes back in the response's structured_output field. If absent, attempt to extract it from the body.
    pub fn json(
        &self,
        prompt: &str,
        system: Option<&str>,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let schema_str = schema.to_string();
        self.with_retry("judge", || {
            let r = self.call_once(prompt, system, Some(&schema_str))?;
            match r.structured {
                Some(v) => Ok(v),
                None => extract_json(&r.text),
            }
        })
    }
}

/// Extract just the JSON object from a response mixed with code fences/chatter (fallback path).
pub fn extract_json(raw: &str) -> Result<serde_json::Value> {
    let t = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        return Ok(v);
    }
    if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(end) = after.find("```") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(after[..end].trim()) {
                return Ok(v);
            }
        }
    }
    if let (Some(s), Some(e)) = (t.find('{'), t.rfind('}')) {
        if s < e {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t[s..=e]) {
                return Ok(v);
            }
        }
    }
    Err(anyhow!("Failed to extract JSON: {}", truncate(t, 300)))
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
