/// Centralized command parser for Telegram messages.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Help,
    Support,
    Admin,
    Unknown(String),
}

/// Parse the first token as a command if it starts with '/'.
/// It strips an optional bot username suffix (e.g. `/start@MyBot`).
pub fn parse_command(text: &str) -> Option<Command> {
    let t = text.trim();
    if !t.starts_with('/') {
        return None;
    }

    let first = t.split_whitespace().next().unwrap_or("");
    let cmd = first.trim_start_matches('/');
    // strip @botname if present
    let cmd = cmd.split('@').next().unwrap_or("").to_lowercase();

    match cmd.as_str() {
        "start" => Some(Command::Start),
        "help" => Some(Command::Help),
        "support" => Some(Command::Support),
        "admin" => Some(Command::Admin),
        other => Some(Command::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_start() {
        assert_eq!(parse_command("/start"), Some(Command::Start));
        assert_eq!(parse_command("/start@MyBot hello"), Some(Command::Start));
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse_command("/help"), Some(Command::Help));
    }

    #[test]
    fn non_command() {
        assert_eq!(parse_command("hello world"), None);
    }
}
