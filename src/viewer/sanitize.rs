use std::collections::HashSet;

use ammonia::Builder;

use crate::config::ViewerRemoteImages;

#[derive(Debug, Clone, Copy)]
pub struct HtmlSanitizer {
    remote_images: ViewerRemoteImages,
}

impl HtmlSanitizer {
    #[must_use]
    pub const fn new(remote_images: ViewerRemoteImages) -> Self {
        Self { remote_images }
    }

    #[must_use]
    pub fn sanitize(&self, html: &str) -> String {
        let mut builder = Builder::new();

        builder
            .add_tags(&[
                "table", "thead", "tbody", "tfoot", "tr", "td", "th", "col", "colgroup",
            ])
            .add_generic_attributes(&["style"])
            .add_tag_attributes(
                "table",
                &[
                    "align",
                    "valign",
                    "border",
                    "cellpadding",
                    "cellspacing",
                    "bgcolor",
                    "width",
                    "height",
                ],
            )
            .add_tag_attributes(
                "td",
                &["colspan", "rowspan", "align", "valign", "width", "height"],
            )
            .add_tag_attributes(
                "th",
                &["colspan", "rowspan", "align", "valign", "width", "height"],
            )
            .add_tag_attributes("img", &["src", "alt", "title", "width", "height"])
            .add_url_schemes(&["cid"]);

        if self.remote_images == ViewerRemoteImages::Block {
            builder.url_schemes(HashSet::from(["cid", "mailto", "tel"]));
        }

        builder.clean(html).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_scripts_and_event_handlers() {
        let sanitizer = HtmlSanitizer::new(ViewerRemoteImages::Allow);

        let sanitized =
            sanitizer.sanitize("<p onclick=\"alert(1)\">Hi</p><script>alert(1)</script>");

        assert!(sanitized.contains("<p>Hi</p>"));
        assert!(!sanitized.contains("onclick"));
        assert!(!sanitized.contains("script"));
    }

    #[test]
    fn can_block_remote_images() {
        let sanitizer = HtmlSanitizer::new(ViewerRemoteImages::Block);

        let sanitized = sanitizer.sanitize("<img src=\"https://example.com/pixel.png\" alt=\"p\">");

        assert!(sanitized.contains("<img"));
        assert!(!sanitized.contains("https://example.com"));
    }
}
