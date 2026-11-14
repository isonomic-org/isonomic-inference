//! Minimal argument parsing: `--name value` pairs and positional values.

use std::collections::HashMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub options: HashMap<String, String>,
    pub positional: Vec<String>,
}

impl Args {
    /// Parse `argv`. Every `--name` listed in `known` takes one value.
    pub fn parse(argv: &[String], known: &[&str]) -> Result<Self, String> {
        let mut args = Args::default();
        let mut iter = argv.iter();
        while let Some(arg) = iter.next() {
            match arg.strip_prefix("--") {
                Some(name) if known.contains(&name) => {
                    let value = iter
                        .next()
                        .ok_or_else(|| format!("--{name} needs a value"))?;
                    args.options.insert(name.to_string(), value.clone());
                }
                Some(name) => return Err(format!("unknown option --{name}")),
                None => args.positional.push(arg.clone()),
            }
        }
        Ok(args)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    pub fn require(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("missing --{name}; run `cargo xtask help`"))
    }

    pub fn number(&self, name: &str, default: u64) -> Result<u64, String> {
        match self.get(name) {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("--{name} must be a whole number, got {raw}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_and_positionals_are_separated() {
        let args = Args::parse(&argv(&["id", "--dir", "/tmp/m"]), &["dir"]).unwrap();
        assert_eq!(args.positional, ["id"]);
        assert_eq!(args.get("dir"), Some("/tmp/m"));
        assert_eq!(args.require("dir"), Ok("/tmp/m"));
        assert_eq!(args.number("max-tokens", 64), Ok(64));
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(Args::parse(&argv(&["--nope", "x"]), &["dir"]).is_err());
        assert!(Args::parse(&argv(&["--dir"]), &["dir"]).is_err());
        let args = Args::parse(&argv(&["--n", "abc"]), &["n"]).unwrap();
        assert!(args.number("n", 1).is_err());
        assert!(args.require("server").is_err());
    }
}
