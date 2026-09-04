use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Diagnostic log verbosity levels (matches shared logging constraints; no separate `warn` control).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const ALL: [LogLevel; 4] = [
        LogLevel::Error,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    pub fn as_filter_directive(self) -> &'static str {
        self.as_str()
    }

    pub fn step(self, delta: i32) -> Self {
        let idx = Self::ALL.iter().position(|l| *l == self).unwrap_or(1) as i32;
        let next = (idx + delta).clamp(0, (Self::ALL.len() - 1) as i32) as usize;
        Self::ALL[next]
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        LogLevel::Info
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Ok(LogLevel::Error),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            "trace" => Ok(LogLevel::Trace),
            other => Err(format!(
                "invalid log level `{other}`; expected error|info|debug|trace"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        assert_eq!("info".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!(LogLevel::Error.to_string(), "error");
        assert!("warn".parse::<LogLevel>().is_err());
    }

    #[test]
    fn step_clamps() {
        assert_eq!(LogLevel::Error.step(-1), LogLevel::Error);
        assert_eq!(LogLevel::Error.step(1), LogLevel::Info);
        assert_eq!(LogLevel::Trace.step(1), LogLevel::Trace);
        assert_eq!(LogLevel::Trace.step(-1), LogLevel::Debug);
    }
}
