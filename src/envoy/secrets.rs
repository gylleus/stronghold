//! Secret-shaped values block drafts without echoing the matched value.

pub fn scan(text: &str) -> Result<(), String> {
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        return Err("Private key detected".into());
    }
    let patterns = [
        ("xoxb-", 15, "Slack token"),
        ("xoxp-", 15, "Slack token"),
        ("xoxa-", 15, "Slack token"),
        ("xoxs-", 15, "Slack token"),
        ("xapp-", 15, "Slack app token"),
        ("xoxe.", 15, "Slack token"),
        ("ghp_", 20, "GitHub token"),
        ("gho_", 20, "GitHub token"),
        ("ghu_", 20, "GitHub token"),
        ("ghs_", 20, "GitHub token"),
        ("ghr_", 20, "GitHub token"),
        ("github_pat_", 20, "GitHub token"),
        ("sk-ant-", 20, "Anthropic token"),
        ("sk-proj-", 20, "API token"),
        ("sk-", 32, "API token"),
        ("AKIA", 20, "AWS access key"),
        ("ASIA", 20, "AWS access key"),
        ("AIza", 30, "Google API key"),
    ];
    for (prefix, min, label) in patterns {
        for (at, _) in text.match_indices(prefix) {
            let len = text[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .count();
            if len >= min {
                return Err(format!("{label} detected"));
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    for label in [
        "aws_secret_access_key",
        "client_secret",
        "api_key",
        "access_token",
        "password",
    ] {
        for (at, _) in lower.match_indices(label) {
            let value = &lower[at + label.len()..];
            let value = value.trim_start_matches([' ', '"', '\'']);
            if let Some(value) = value.strip_prefix(['=', ':']) {
                let value = value.trim_start_matches([' ', '"', '\'']);
                let len = value
                    .chars()
                    .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | ',' | ';'))
                    .count();
                if len >= 16 {
                    return Err(format!("Credential assignment ({label}) detected"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_credentials_without_echoing_them() {
        for value in [
            "xoxb-12345678901234567890",
            "ghp_123456789012345678901234567890",
            "sk-ant-api03-12345678901234567890",
            "AKIAIOSFODNN7EXAMPLE",
            "-----BEGIN RSA PRIVATE KEY-----",
            "aws_secret_access_key=abcdefghijklmnopqrstuvwx",
            "token=ghp_123456789012345678901234567890",
            "api_key is documented. api_key=abcdefghijklmnopqrstuvwx",
        ] {
            let error = scan(value).unwrap_err();
            assert!(!error.contains(value));
        }
    }
    #[test]
    fn normal_prose_and_slack_ids_are_not_secrets() {
        for value in [
            "Ask U000KARL001 in C000AGENT01",
            "The api_key needs rotating.",
            "Please reset your password.",
            "The project is ready for review.",
            "https://example.slack.com/archives/C123/p1790000000000000",
        ] {
            assert!(scan(value).is_ok(), "{value}");
        }
    }
}
