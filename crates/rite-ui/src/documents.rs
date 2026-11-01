//! The documents reader's markdown (ticket 129), rendered here rather than
//! in the page so that images can be read from the repository and inlined.
//!
//! The page puts this HTML straight into the reader, so nothing in a
//! document may run or navigate: raw HTML is shown as text, a link to
//! another document becomes `#doc:<path>` for the page to open in the
//! reader, a web or mail link keeps its address for the page to hand to
//! the system, anything else (`javascript:`, `file:`) goes nowhere, and an
//! image is inlined from the repository or left out.

use std::path::Path;

use base64::Engine;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use rite_lib::documents;

/// `text`, the document at `path` in the repository `root`, as HTML.
pub fn render(root: &Path, path: &str, text: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut in_metadata = false;
    let events = Parser::new_ext(text, options).filter_map(|event| match event {
        // Frontmatter isn't part of the document as read.
        Event::Start(Tag::MetadataBlock(_)) => {
            in_metadata = true;
            None
        }
        Event::End(TagEnd::MetadataBlock(_)) => {
            in_metadata = false;
            None
        }
        _ if in_metadata => None,
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: link(path, &dest_url).into(),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: image(root, path, &dest_url),
            title,
            id,
        })),
        other => Some(other),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// A release's notes from GitHub (ticket 336), as HTML for What's new. The
/// same rules as a document, with nothing in a repository to point at: raw
/// HTML is text, only web and mail links keep their address, images are
/// left out (their text stays), and a bare `https://` address, which
/// GitHub's generated notes are full of, becomes a link. Every heading is an
/// `h4`, so the notes' own "What's Changed" sits under the release's name.
pub fn render_release_notes(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut in_link = false;
    let events = Parser::new_ext(text, options).flat_map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => vec![Event::Text(raw)],
        Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => Vec::new(),
        Event::Start(Tag::Heading {
            id, classes, attrs, ..
        }) => vec![Event::Start(Tag::Heading {
            level: HeadingLevel::H4,
            id,
            classes,
            attrs,
        })],
        Event::End(TagEnd::Heading(_)) => vec![Event::End(TagEnd::Heading(HeadingLevel::H4))],
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            in_link = true;
            vec![Event::Start(Tag::Link {
                link_type,
                dest_url: web_link(&dest_url).into(),
                title,
                id,
            })]
        }
        Event::End(TagEnd::Link) => {
            in_link = false;
            vec![Event::End(TagEnd::Link)]
        }
        Event::Text(text) if !in_link => autolink(&text),
        other => vec![other],
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// A web or mail address kept; anything else goes nowhere.
fn web_link(dest: &str) -> String {
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        dest.to_string()
    } else {
        "#".to_string()
    }
}

/// `text` with each bare `https://` address made a link to itself; a full
/// stop, comma or bracket after one is left outside it.
fn autolink(text: &str) -> Vec<Event<'static>> {
    let mut events = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("https://") {
        let tail = &rest[at..];
        let len = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let url = tail[..len].trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
        if url.len() <= "https://".len() {
            events.push(Event::Text(rest[..at + len].to_string().into()));
            rest = &rest[at + len..];
            continue;
        }
        if at > 0 {
            events.push(Event::Text(rest[..at].to_string().into()));
        }
        let url: CowStr<'static> = url.to_string().into();
        events.push(Event::Start(Tag::Link {
            link_type: pulldown_cmark::LinkType::Autolink,
            dest_url: url.clone(),
            title: "".into(),
            id: "".into(),
        }));
        events.push(Event::Text(url.clone()));
        events.push(Event::End(TagEnd::Link));
        rest = &rest[at + url.len()..];
    }
    if !rest.is_empty() {
        events.push(Event::Text(rest.to_string().into()));
    }
    events
}

/// Where a link in the document `from` should go.
fn link(from: &str, dest: &str) -> String {
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        return dest.to_string();
    }
    // A heading in this document: left as it is, harmless.
    if dest.starts_with('#') {
        return dest.to_string();
    }
    match documents::join_link(from, &decode(dest)) {
        Some(target) => format!("#doc:{target}"),
        None => "#".to_string(),
    }
}

/// An image's source: a picture in the repository, inlined; anything else
/// (a web address the page's policy wouldn't load anyway) left out.
fn image(root: &Path, from: &str, dest: &str) -> CowStr<'static> {
    let Some(target) = documents::join_link(from, &decode(dest)) else {
        return "".into();
    };
    let Some(mime) = mime(&target) else {
        return "".into();
    };
    match documents::read_bytes(root, &target) {
        Ok(bytes) => format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
        .into(),
        Err(_) => "".into(),
    }
}

fn mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        // Shown through <img>, where an SVG's scripts never run.
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// `%20` and friends, as a link in markdown often has them.
fn decode(dest: &str) -> String {
    let bytes = dest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = dest.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_notes_link_bare_addresses_and_run_nothing() {
        let html = render_release_notes(
            "## What's Changed\n* fix: a thing by @me in https://github.com/KanRite/rite/pull/273.\n\n\
             <script>alert(1)</script>\n\n[x](javascript:alert(1)) ![pic](https://x/p.png) [ok](https://x/y)",
        );
        assert!(html.contains("<h4>What's Changed</h4>"), "{html}");
        assert!(
            html.contains(r#"in <a href="https://github.com/KanRite/rite/pull/273">https://github.com/KanRite/rite/pull/273</a>.</li>"#),
            "{html}"
        );
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains(r##"<a href="#">x</a>"##), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains(r#"<a href="https://x/y">ok</a>"#), "{html}");
    }

    #[test]
    fn an_address_inside_a_link_is_not_linked_again() {
        let html = render_release_notes("[https://x/a](https://x/a) and https://");
        assert_eq!(html.matches("<a ").count(), 1, "{html}");
        assert!(html.contains("and https://</p>"), "{html}");
    }

    #[test]
    fn raw_html_is_text() {
        let html = render(
            Path::new("/nowhere"),
            "a.md",
            "<script>alert(1)</script>\n\nhi <b>x</b>\n",
        );
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("&lt;b&gt;"), "{html}");
    }

    #[test]
    fn links_are_tamed() {
        let html = render(
            Path::new("/nowhere"),
            "rfc/0002.md",
            "[a](../adr/0001.md) [b](https://example.com) [c](javascript:alert(1)) [d](../../out.md)\n",
        );
        assert!(html.contains(r##"href="#doc:adr/0001.md""##), "{html}");
        assert!(html.contains(r#"href="https://example.com""#), "{html}");
        assert!(!html.contains("javascript"), "{html}");
        assert_eq!(html.matches(r##"href="#""##).count(), 2, "{html}");
    }

    #[test]
    fn tables_tasks_and_frontmatter() {
        let html = render(
            Path::new("/nowhere"),
            "a.md",
            "---\nstatus: draft\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n",
        );
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("checkbox"), "{html}");
        assert!(!html.contains("status: draft"), "{html}");
    }

    #[test]
    fn images_are_inlined_from_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("img")).unwrap();
        std::fs::write(dir.path().join("img/p.png"), b"PNG").unwrap();
        let html = render(
            dir.path(),
            "docs/a.md",
            "![p](../img/p.png) ![w](https://example.com/x.png)\n",
        );
        assert!(html.contains("data:image/png;base64,UE5H"), "{html}");
        assert!(!html.contains("example.com"), "{html}");
    }
}
