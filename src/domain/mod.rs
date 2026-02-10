pub mod diff;
pub mod model;
pub mod registry;

/// Convert PascalCase / camelCase to snake_case.
pub fn to_snake(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_uppercase() && i > 0 {
            let prev_lower = chars[i - 1].is_lowercase();
            let next_lower = chars.get(i + 1).map_or(false, |c| c.is_lowercase());
            if prev_lower || next_lower {
                result.push('_');
            }
        }
        result.push(ch.to_ascii_lowercase());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::to_snake;

    #[test]
    fn test_to_snake_simple() {
        assert_eq!(to_snake("UserService"), "user_service");
    }

    #[test]
    fn test_to_snake_consecutive_caps() {
        assert_eq!(to_snake("UserID"), "user_id");
        assert_eq!(to_snake("HTMLParser"), "html_parser");
        assert_eq!(to_snake("getHTTPResponse"), "get_http_response");
    }

    #[test]
    fn test_to_snake_already_lower() {
        assert_eq!(to_snake("already_snake"), "already_snake");
    }
}
