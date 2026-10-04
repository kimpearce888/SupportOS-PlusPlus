//! SafeHtml — render sanitized thread HTML (reference
//! components/common/SafeHtml.tsx).
//!
//! SAFETY MODEL: this component is only safe because the SERVER whitelist
//! (core security::sanitize_thread_html, the ammonia port of the reference's
//! sanitizeThreadHtml) strips scripts, event handlers, javascript: URLs and
//! dangerous CSS BEFORE the HTML reaches the client. `inner_html` itself
//! provides NO protection — do not reuse this component for HTML from any
//! other source.
//!
//! The reference renders `body_html` via dangerouslySetInnerHTML and falls
//! back to `body_text` split into paragraphs. The port's thread `body` field
//! carries BOTH shapes (sanitized HTML for email threads, plain text
//! otherwise), so the component routes on a tag-shape check: HTML renders as
//! HTML, plain text renders as paragraphs — the same observable behavior.

use leptos::*;

/// Split text into paragraphs on 2+ newlines (reference
/// `renderParagraphs`: `text.split(/\n{2,}/).filter(non-empty)`).
#[must_use]
pub fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether the string looks like HTML (contains a tag) rather than plain
/// text. Sanitizer output for plain-text input contains no tags, so this
/// routes the two body shapes exactly like the reference's html/text split.
#[must_use]
pub fn looks_like_html(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1].is_ascii_alphabetic() {
            // Find the closing '>' within a sane tag length.
            for j in i + 2..(i + 120).min(bytes.len()) {
                if bytes[j] == b'>' {
                    return true;
                }
                if bytes[j] == b'<' {
                    break;
                }
            }
        }
        i += 1;
    }
    false
}

/// Render sanitized thread HTML (reference `SafeHtml`).
#[component]
pub fn SafeHtml(html: Option<String>, fallback_text: String) -> impl IntoView {
    let raw = html.unwrap_or_default();
    let trimmed = raw.trim();
    if !trimmed.is_empty() && looks_like_html(trimmed) {
        view! { <div class="spp-thread-body" inner_html=raw></div> }
    } else {
        let text = if trimmed.is_empty() {
            fallback_text
        } else {
            raw.clone()
        };
        view! {
            <div class="spp-thread-body">
                {split_paragraphs(&text)
                    .into_iter()
                    .map(|p| view! { <p>{p}</p> })
                    .collect::<Vec<_>>()}
            </div>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_paragraphs_matches_reference_regex() {
        assert_eq!(
            split_paragraphs("one\n\ntwo\n\n\n\nthree"),
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
        assert_eq!(split_paragraphs("single"), vec!["single".to_string()]);
        assert!(split_paragraphs("  \n\n  ").is_empty());
        // Single newlines stay inside the paragraph (only 2+ split).
        assert_eq!(
            split_paragraphs("line1\nline2\n\nline3"),
            vec!["line1\nline2".to_string(), "line3".to_string()]
        );
    }

    #[test]
    fn looks_like_html_detects_tags_not_prose() {
        assert!(looks_like_html("<p>hello</p>"));
        assert!(looks_like_html("Hi <b>team</b>, see below"));
        assert!(looks_like_html("<br/>"));
        assert!(!looks_like_html("plain text with < not a tag"));
        assert!(!looks_like_html("a < b and 5 > 3"));
        assert!(!looks_like_html(""));
    }
}
