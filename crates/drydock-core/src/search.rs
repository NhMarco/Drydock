//! Matching what someone types against what a store title actually says.
//!
//! Titles carry marks and punctuation nobody types: `EA SPORTS FC™ 27`, `Marvel's Spider-Man` with
//! a curly apostrophe, `Pokémon`, `Yakuza: Like a Dragon`, `Ōkami`. Comparing them verbatim means a
//! search for the name on the box finds nothing at all — so both sides are reduced to the same plain
//! form before they are compared.
//!
//! The reduction is deliberately blunt:
//!
//! * letters and digits are kept, lowercased, with the common accents folded to their base letter
//!   (`é` → `e`, `ß` → `ss`); anything the table does not know keeps its own lowercase form, so a
//!   Japanese or Cyrillic title still matches itself;
//! * everything else — `™`, `®`, `:`, `-`, `'`, `,` — becomes a single space;
//! * and a second, spaceless form lets `fc27` find `FC™ 27`.
//!
//! The plain ASCII case is checked first without allocating, because that is nearly every search.

/// What the user typed, prepared once so each candidate is only compared against it.
///
/// A search runs over the whole catalogue — a quarter of a million titles — on every keystroke, so
/// nothing here allocates per candidate: the title is normalised as it is read and matched against
/// the prepared needle in one pass.
#[derive(Clone, Debug)]
pub struct SearchQuery {
    raw: String,
    /// The normalised needle and its Knuth-Morris-Pratt table, so a title can be matched from a
    /// stream of characters without ever being collected into a string.
    needle: Vec<char>,
    needle_skip: Vec<usize>,
    /// The same without spaces, so `fc27` finds `FC™ 27`.
    compact: Vec<char>,
    compact_skip: Vec<usize>,
}

impl SearchQuery {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let needle: Vec<char> = Normalized::new(text).collect();
        let compact: Vec<char> = needle.iter().copied().filter(|c| *c != ' ').collect();
        Self {
            raw: text.trim().to_lowercase(),
            needle_skip: skip_table(&needle),
            compact_skip: skip_table(&compact),
            needle,
            compact,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty() && self.raw.is_empty()
    }

    /// What was typed, trimmed and lowercased — for the callers that match an App ID or compare
    /// against a name they already hold.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.raw
    }

    /// Whether `name` is a hit. An App ID typed in full is matched by the caller, which has it.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        // Nearly every search is plain text against a plain title, and this answers it by scanning
        // the bytes as they are.
        if contains_ignore_ascii_case(name, &self.raw) {
            return true;
        }
        if self.needle.is_empty() {
            return false;
        }
        if contains(Normalized::new(name), &self.needle, &self.needle_skip) {
            return true;
        }
        // Last chance: the user left out a space the title has, or put in one it has not.
        !self.compact.is_empty()
            && contains(
                Normalized::new(name).filter(|c| *c != ' '),
                &self.compact,
                &self.compact_skip,
            )
    }
}

/// How far the match can restart after a mismatch (the Knuth-Morris-Pratt failure function), so a
/// title is read once, forwards, whatever the needle repeats.
fn skip_table(needle: &[char]) -> Vec<usize> {
    let mut table = vec![0; needle.len()];
    let mut prefix = 0;
    for index in 1..needle.len() {
        while prefix > 0 && needle[index] != needle[prefix] {
            prefix = table[prefix - 1];
        }
        if needle[index] == needle[prefix] {
            prefix += 1;
        }
        table[index] = prefix;
    }
    table
}

/// Whether the characters `haystack` yields contain `needle`.
fn contains(haystack: impl Iterator<Item = char>, needle: &[char], skip: &[usize]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut matched = 0;
    for character in haystack {
        while matched > 0 && character != needle[matched] {
            matched = skip[matched - 1];
        }
        if character == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                return true;
            }
        }
    }
    false
}

/// A title's characters in their plain form: letters and digits lowercased and folded, every run of
/// anything else a single space, no space at the start.
struct Normalized<'a> {
    source: std::str::Chars<'a>,
    /// Characters produced from one source character (`ß` → `ss`, an uppercase form → its lowercase).
    queued: [char; 4],
    queued_len: usize,
    queued_at: usize,
    space_pending: bool,
    emitted: bool,
}

impl<'a> Normalized<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            source: text.chars(),
            queued: [' '; 4],
            queued_len: 0,
            queued_at: 0,
            space_pending: false,
            emitted: false,
        }
    }

    fn queue(&mut self, characters: impl Iterator<Item = char>) {
        self.queued_len = 0;
        self.queued_at = 0;
        for character in characters.take(self.queued.len()) {
            self.queued[self.queued_len] = character;
            self.queued_len += 1;
        }
    }
}

impl Iterator for Normalized<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        loop {
            if self.queued_at < self.queued_len {
                let character = self.queued[self.queued_at];
                self.queued_at += 1;
                self.emitted = true;
                return Some(character);
            }
            let character = self.source.next()?;
            if let Some(folded) = fold(character) {
                self.queue(folded.chars());
            } else if character.is_alphanumeric() {
                self.queue(character.to_lowercase());
            } else {
                self.space_pending = true;
                continue;
            }
            if self.space_pending && self.emitted {
                self.space_pending = false;
                self.emitted = true;
                return Some(' ');
            }
            self.space_pending = false;
        }
    }
}

/// Lowercase, accents folded, everything that is not a letter or digit turned into one space.
/// Matching does not go through this — it reads the same characters straight off the title — but a
/// caller that wants the plain form (or a test that wants to read it) gets it here.
#[must_use]
pub fn normalize(text: &str) -> String {
    Normalized::new(text).collect()
}

/// The base letters of the accented forms that turn up in game titles. Anything not listed keeps
/// its own lowercase form, which is right for scripts that have no such base letter.
fn fold(character: char) -> Option<&'static str> {
    Some(match character {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā'
        | 'Ă' | 'Ą' => "a",
        'æ' | 'Æ' => "ae",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' | 'Ç' | 'Ć' | 'Č' | 'Ĉ' | 'Ċ' => "c",
        'ď' | 'đ' | 'Ď' | 'Đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė'
        | 'Ę' | 'Ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'į' | 'ı' | 'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Į' | 'İ' => {
            "i"
        }
        'ł' | 'ĺ' | 'ľ' | 'Ł' | 'Ĺ' | 'Ľ' => "l",
        'ñ' | 'ń' | 'ň' | 'ņ' | 'Ñ' | 'Ń' | 'Ň' | 'Ņ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō'
        | 'Ŏ' | 'Ő' => "o",
        'œ' | 'Œ' => "oe",
        'ŕ' | 'ř' | 'Ŕ' | 'Ř' => "r",
        'ś' | 'ş' | 'š' | 'ŝ' | 'Ś' | 'Ş' | 'Š' | 'Ŝ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'Ţ' | 'Ť' | 'Ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū'
        | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => "u",
        'ý' | 'ÿ' | 'ŷ' | 'Ý' | 'Ÿ' | 'Ŷ' => "y",
        'ź' | 'ż' | 'ž' | 'Ź' | 'Ż' | 'Ž' => "z",
        _ => return None,
    })
}

/// `haystack.to_lowercase().contains(needle)` for an already-lowercase ASCII `needle`, without
/// building the lowercase copy. Falls out to `false` for a needle that is not plain ASCII, which
/// the normalised comparison then handles.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if !needle.is_ascii() {
        return false;
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The titles that sent us here: every one of these was typed as it is spoken and found nothing.
    #[test]
    fn a_title_is_found_by_what_is_on_the_box() {
        let hit = |query: &str, name: &str| SearchQuery::new(query).matches(name);
        assert!(hit("ea sports fc 27", "EA SPORTS FC™ 27"));
        assert!(hit("EA Sports FC 27", "EA SPORTS FC™ 27"));
        assert!(hit("fc 27", "EA SPORTS FC™ 27"));
        assert!(
            hit("fc27", "EA SPORTS FC™ 27"),
            "the space in the title is optional"
        );
        assert!(hit("pokemon", "Pokémon™ Legends"));
        assert!(hit("marvels spider man", "Marvel's Spider-Man Remastered"));
        assert!(hit("marvel's spider-man", "Marvel's Spider-Man Remastered"));
        assert!(hit("yakuza like a dragon", "Yakuza: Like a Dragon"));
        assert!(hit("okami", "Ōkami HD"));
        assert!(hit("tom clancys", "Tom Clancy's® The Division® 2"));
        assert!(hit("witcher 3", "The Witcher® 3: Wild Hunt"));
        assert!(hit("koln", "Köln Simulator"));
    }

    #[test]
    fn a_search_still_tells_games_apart() {
        let hit = |query: &str, name: &str| SearchQuery::new(query).matches(name);
        assert!(!hit("fc 26", "EA SPORTS FC™ 27"));
        assert!(!hit("spider man", "Marvel Rivals"));
        assert!(!hit("portal", "Half-Life 2"));
        assert!(
            SearchQuery::new("  ").matches("anything"),
            "an empty search matches everything"
        );
    }

    #[test]
    fn a_repeating_query_is_matched_where_a_naive_scan_would_give_up() {
        // "aab" sits at the end of "aaab": a matcher that restarts at the beginning after a
        // mismatch walks past it. The title is read once, so the restart has to be the right one.
        assert!(SearchQuery::new("aab").matches("Aaab"));
        assert!(SearchQuery::new("ab ab c").matches("™ab ab ab c!"));
        assert!(!SearchQuery::new("aabb").matches("Aaab"));
    }

    #[test]
    fn marks_and_punctuation_become_one_space_each() {
        assert_eq!(normalize("EA SPORTS FC™ 27"), "ea sports fc 27");
        assert_eq!(normalize("Marvel's  Spider-Man"), "marvel s spider man");
        assert_eq!(normalize("  ™®©  "), "");
        assert_eq!(normalize("Pokémon"), "pokemon");
        assert_eq!(normalize("Straße 27"), "strasse 27");
        // A script with no Latin base letter keeps itself rather than vanishing.
        assert_eq!(normalize("ペルソナ5"), "ペルソナ5");
    }
}
