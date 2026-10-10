// SPDX-License-Identifier: GPL-3.0-or-later
//! Which of the types a selection is offered in gets recorded.
//!
//! A copy is offered in several types at once -- a browser's text selection comes as
//! `text/html`, `text/plain;charset=utf-8`, `UTF8_STRING` and more -- and the history keeps one.
//! Plain text wins over an image when both are offered, as Klipper decides it: a spreadsheet's
//! cells come with a picture of themselves attached, and the text is what anyone pasting them
//! back wants.
//!
//! What is lost is the rich half -- the HTML, the spreadsheet's own format. Putting an entry
//! back offers plain text, or the image in the type it came in, and nothing more.

/// The types plain text is offered in, best first, and the types it is put back in.
///
/// `text/plain;charset=utf-8` is the modern spelling, `text/plain` the one some toolkits still
/// use, and the last three are X11 selection targets that Xwayland and older toolkits ask for.
/// `STRING` and `TEXT` are nominally Latin-1; text read through them is decoded leniently.
pub const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// The image type preferred when several are offered. Everything that takes an image from a
/// clipboard takes this, so putting a PNG back loses nothing in practice.
const PREFERRED_IMAGE: &str = "image/png";

/// Offered by every selection this program sets, so that its own selection, coming back round
/// as a "new" one, is recognized and not recorded a second time.
pub const MARKER: &str = "application/x-wlrix-clipboard";

/// Set by password managers -- KeePassXC, among others -- on a copied password. The value is
/// `secret`; its presence is enough. Such a selection is never recorded: the history is written
/// to disk, and a password in it would sit there until it was evicted.
pub const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// What to do with a selection.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Read it as text, through this type.
    Text(String),
    /// Read it as an image, through this type.
    Image(String),
    /// Leave it alone.
    Ignore(Reason),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    /// It is this program's own selection.
    Own,
    /// A password manager asked for it not to be kept.
    Secret,
    /// Nothing in it is text or an image.
    Unsupported,
}

/// Decide what to record from a selection offered in `types`.
pub fn choose(types: &[String]) -> Choice {
    let offered = |wanted: &str| types.iter().any(|mime| mime == wanted);
    if offered(MARKER) {
        return Choice::Ignore(Reason::Own);
    }
    if offered(PASSWORD_HINT) {
        return Choice::Ignore(Reason::Secret);
    }
    if let Some(text) = TEXT_TYPES.iter().find(|mime| offered(mime)) {
        return Choice::Text((*text).to_owned());
    }
    if offered(PREFERRED_IMAGE) {
        return Choice::Image(PREFERRED_IMAGE.to_owned());
    }
    if let Some(image) = types.iter().find(|mime| mime.starts_with("image/")) {
        return Choice::Image(image.clone());
    }
    Choice::Ignore(Reason::Unsupported)
}

/// The file extension an image type is stored under, so the blobs can be opened by hand.
pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choose_from(types: &[&str]) -> Choice {
        choose(
            &types
                .iter()
                .map(|mime| (*mime).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn the_modern_text_type_wins() {
        assert_eq!(
            choose_from(&["text/html", "UTF8_STRING", "text/plain;charset=utf-8"]),
            Choice::Text("text/plain;charset=utf-8".to_owned())
        );
        assert_eq!(
            choose_from(&["STRING", "UTF8_STRING"]),
            Choice::Text("UTF8_STRING".to_owned())
        );
    }

    #[test]
    fn text_wins_over_an_image() {
        assert_eq!(
            choose_from(&["image/png", "text/plain"]),
            Choice::Text("text/plain".to_owned())
        );
    }

    #[test]
    fn png_wins_among_images() {
        assert_eq!(
            choose_from(&["image/bmp", "image/png", "image/jpeg"]),
            Choice::Image("image/png".to_owned())
        );
        assert_eq!(
            choose_from(&["text/html", "image/jpeg"]),
            Choice::Image("image/jpeg".to_owned())
        );
    }

    #[test]
    fn passwords_are_not_recorded() {
        assert_eq!(
            choose_from(&["text/plain", PASSWORD_HINT]),
            Choice::Ignore(Reason::Secret)
        );
    }

    #[test]
    fn our_own_selection_is_not_recorded_again() {
        assert_eq!(
            choose_from(&["text/plain", MARKER]),
            Choice::Ignore(Reason::Own)
        );
    }

    #[test]
    fn anything_else_is_left_alone() {
        assert_eq!(
            choose_from(&["text/uri-list", "application/x-kde-cutselection"]),
            Choice::Ignore(Reason::Unsupported)
        );
        assert_eq!(choose_from(&[]), Choice::Ignore(Reason::Unsupported));
    }
}
