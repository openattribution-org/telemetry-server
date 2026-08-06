use dashmap::DashMap;
use url::Url;
use uuid::Uuid;

/// Extract the registrable domain from a URL.
///
/// Strips `www.` prefix and returns the hostname. For subdomain matching,
/// we check both the exact domain and parent domains against the index.
pub fn extract_domain(url_str: &str) -> Option<String> {
    let url = Url::parse(url_str).ok()?;
    let host = url.host_str()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    Some(host.to_lowercase())
}

/// Look up a domain in the index, walking up the hierarchy.
///
/// Tries exact match first, then parent domain (e.g. `tech.ft.com` -> `ft.com`).
pub fn resolve_domain(index: &DashMap<String, Uuid>, domain: &str) -> Option<Uuid> {
    if let Some(entry) = index.get(domain) {
        return Some(*entry.value());
    }

    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() > 2 {
        let parent = parts[1..].join(".");
        if let Some(entry) = index.get(&parent) {
            return Some(*entry.value());
        }
    }

    None
}

/// Check whether any of the given domains match the given URL.
pub fn url_matches_domains(url_str: &str, domains: &[String]) -> bool {
    let Some(domain) = extract_domain(url_str) else {
        return false;
    };

    for registered in domains {
        let registered = registered
            .strip_prefix("www.")
            .unwrap_or(registered)
            .to_lowercase();
        if domain == registered || domain.ends_with(&format!(".{registered}")) {
            return true;
        }
    }

    false
}

/// Generate SQL LIKE patterns for domain-based content_url filtering.
pub fn domain_like_patterns(domains: &[&str]) -> Vec<String> {
    domains
        .iter()
        .flat_map(|d| {
            vec![
                format!("https://{d}/%"),
                format!("https://www.{d}/%"),
                format!("http://{d}/%"),
                format!("http://www.{d}/%"),
            ]
        })
        .collect()
}

/// Filter a list of domains by an optional domain filter.
///
/// If `domain_filter` is Some, only returns domains that match or are
/// subdomains of the filter. If None, returns all domains.
pub fn effective_domains<'a>(
    org_domains: &'a [String],
    domain_filter: Option<&str>,
) -> Vec<&'a str> {
    match domain_filter {
        Some(filter) => org_domains
            .iter()
            .filter(|d| {
                let d_lower = d.to_lowercase();
                let f_lower = filter.to_lowercase();
                d_lower == f_lower || d_lower.ends_with(&format!(".{f_lower}"))
            })
            .map(String::as_str)
            .collect(),
        None => org_domains.iter().map(String::as_str).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_domain_basic() {
        assert_eq!(
            extract_domain("https://www.bbc.co.uk/news"),
            Some("bbc.co.uk".to_string())
        );
    }

    #[test]
    fn test_extract_domain_subdomain() {
        assert_eq!(
            extract_domain("https://tech.ft.com/article"),
            Some("tech.ft.com".to_string())
        );
    }

    #[test]
    fn test_extract_domain_no_www() {
        assert_eq!(
            extract_domain("https://example.com/page"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn test_extract_domain_invalid() {
        assert_eq!(extract_domain("not-a-url"), None);
    }

    #[test]
    fn test_url_matches_domains() {
        let domains = vec!["ft.com".to_string(), "bbc.co.uk".to_string()];
        assert!(url_matches_domains("https://www.ft.com/article", &domains));
        assert!(url_matches_domains("https://tech.ft.com/page", &domains));
        assert!(url_matches_domains("https://www.bbc.co.uk/news", &domains));
        assert!(!url_matches_domains("https://guardian.com/news", &domains));
    }

    #[test]
    fn test_resolve_domain_exact_match() {
        // GIVEN an index with ft.com
        let index = DashMap::new();
        index.insert("ft.com".to_string(), Uuid::new_v4());

        // WHEN resolving ft.com
        // SHOULD find it
        assert!(resolve_domain(&index, "ft.com").is_some());
    }

    #[test]
    fn test_resolve_domain_parent_fallback() {
        // GIVEN an index with ft.com
        let org_id = Uuid::new_v4();
        let index = DashMap::new();
        index.insert("ft.com".to_string(), org_id);

        // WHEN resolving tech.ft.com (not in index)
        // SHOULD fall back to parent ft.com
        assert_eq!(resolve_domain(&index, "tech.ft.com"), Some(org_id));
    }

    #[test]
    fn test_resolve_domain_miss() {
        // GIVEN an index with ft.com
        let index = DashMap::new();
        index.insert("ft.com".to_string(), Uuid::new_v4());

        // WHEN resolving a completely different domain
        // SHOULD return None
        assert!(resolve_domain(&index, "guardian.com").is_none());
    }

    #[test]
    fn test_resolve_domain_two_level_tld() {
        // GIVEN an index with bbc.co.uk
        let org_id = Uuid::new_v4();
        let index = DashMap::new();
        index.insert("bbc.co.uk".to_string(), org_id);

        // WHEN resolving www.bbc.co.uk (parent = bbc.co.uk)
        // SHOULD find via parent fallback
        assert_eq!(resolve_domain(&index, "www.bbc.co.uk"), Some(org_id));
    }

    #[test]
    fn test_domain_like_patterns_generates_all_variants() {
        // GIVEN a domain
        let patterns = domain_like_patterns(&["ft.com"]);

        // SHOULD generate https, https+www, http, http+www variants
        assert_eq!(patterns.len(), 4);
        assert!(patterns.contains(&"https://ft.com/%".to_string()));
        assert!(patterns.contains(&"https://www.ft.com/%".to_string()));
        assert!(patterns.contains(&"http://ft.com/%".to_string()));
        assert!(patterns.contains(&"http://www.ft.com/%".to_string()));
    }

    #[test]
    fn test_effective_domains_no_filter() {
        // GIVEN org domains
        let domains = vec!["ft.com".to_string(), "tech.ft.com".to_string()];

        // WHEN no filter applied
        let result = effective_domains(&domains, None);

        // SHOULD return all
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_effective_domains_with_filter() {
        // GIVEN org domains
        let domains = vec![
            "ft.com".to_string(),
            "tech.ft.com".to_string(),
            "bbc.co.uk".to_string(),
        ];

        // WHEN filtering for ft.com
        let result = effective_domains(&domains, Some("ft.com"));

        // SHOULD return ft.com and tech.ft.com (subdomain match)
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"ft.com"));
        assert!(result.contains(&"tech.ft.com"));
    }

    #[test]
    fn test_extract_domain_http_scheme() {
        // GIVEN an http URL (not https)
        // SHOULD still extract the domain
        assert_eq!(
            extract_domain("http://example.com/path"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn test_extract_domain_with_port() {
        // GIVEN a URL with a port
        // SHOULD extract the domain without the port
        assert_eq!(
            extract_domain("https://example.com:8080/path"),
            Some("example.com".to_string())
        );
    }
}
