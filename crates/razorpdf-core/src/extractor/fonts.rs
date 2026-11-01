//! Font decoding: `/ToUnicode` CMaps, `/Encoding` differences, AGL, NFKC.
//!
//! Resolution order per code follows the PDF spec: ToUnicode CMap first,
//! then Differences names via the Adobe Glyph List, then the base encoding
//! table (WinAnsi / Standard / MacRoman). Unmapped codes become U+FFFD
//! (explicit damage marker — never raw control bytes), and decoded spans
//! are NFKC-normalized so ligatures (`ﬁ` → `fi`) stay LLM-readable.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use unicode_normalization::UnicodeNormalization;

/// How text-showing bytes decode to Unicode for one font.
#[derive(Debug, Clone)]
pub struct FontDecoding {
    /// Direct ToUnicode entries: code → unicode string.
    direct: HashMap<u32, String>,
    /// Increment-form bfrange rules: (lo, hi, dst_base, dst_width_bytes).
    /// Stored unexpanded so huge CJK ranges cost O(1).
    range_rules: Vec<(u32, u32, u32, usize)>,
    /// Candidate code lengths in bytes, longest first (from codespacerange
    /// or Identity-H/V). Empty means single-byte.
    code_lengths: Vec<usize>,
    /// Single-byte codes via base encoding + Differences. The base tables
    /// are process-wide shared `Arc`s (a refcount bump per font — no
    /// 256-`String` copies on the hot path); Differences clone-on-write.
    byte_map: Arc<[Option<String>; 256]>,
}

impl Default for FontDecoding {
    /// Unknown fonts decode as single-byte WinAnsi (sane ASCII baseline).
    fn default() -> Self {
        Self {
            direct: HashMap::new(),
            range_rules: Vec::new(),
            code_lengths: Vec::new(),
            byte_map: winansi_shared(),
        }
    }
}

impl FontDecoding {
    /// Start from a named base encoding (`WinAnsi`, `Standard`, `MacRoman`,
    /// `PDFDoc` falls back to WinAnsi).
    #[must_use]
    pub fn with_base_encoding(name: &str) -> Self {
        let mut decoding = Self::default();
        match name {
            "MacRomanEncoding" | "MacRoman" => decoding.byte_map = macroman_shared(),
            "StandardEncoding" | "Standard" => decoding.byte_map = standard_shared(),
            _ => {}
        }
        decoding
    }

    /// Apply `/Differences`: `code` names resolve via AGL; unknown names are
    /// skipped so the base table value for that slot survives.
    pub fn apply_differences(&mut self, differences: &[(u8, String)]) {
        for (code, name) in differences {
            if let Some(mapped) = agl_lookup(name) {
                Arc::make_mut(&mut self.byte_map)[*code as usize] = Some(mapped);
            }
        }
    }

    /// Merge a parsed ToUnicode CMap (direct map + range rules + lengths).
    pub fn apply_tounicode(&mut self, cmap: &ToUnicodeCMap) {
        self.direct
            .extend(cmap.direct.iter().map(|(k, v)| (*k, v.clone())));
        self.range_rules.extend(cmap.range_rules.iter().copied());
        if !cmap.code_lengths.is_empty() {
            self.code_lengths = cmap.code_lengths.clone();
        }
    }

    /// Force multi-byte code consumption (Identity-H/V without a CMap).
    pub fn set_two_byte(&mut self) {
        self.code_lengths = vec![2];
    }

    /// Decode text-showing bytes to NFKC-normalized Unicode.
    #[must_use]
    pub fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        // Fast path: plain single-byte fonts skip the CMap machinery
        // entirely (no per-byte HashMap hashing on empty maps).
        if self.direct.is_empty() && self.range_rules.is_empty() && self.code_lengths.is_empty() {
            for &b in bytes {
                match &self.byte_map.as_ref()[b as usize] {
                    Some(text) => out.push_str(text),
                    None => out.push('\u{FFFD}'),
                }
            }
        } else {
            let lengths: &[usize] = if self.code_lengths.is_empty() {
                &[1]
            } else {
                &self.code_lengths
            };
            let mut i = 0;
            while i < bytes.len() {
                let mut matched = false;
                for &n in lengths {
                    if i + n > bytes.len() {
                        continue;
                    }
                    let code = be_int(&bytes[i..i + n]);
                    if let Some(text) = self.lookup(code, n) {
                        out.push_str(&text);
                        i += n;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    out.push('\u{FFFD}');
                    i += 1;
                }
            }
        }
        // NFKC is the identity on ASCII: skip the pass for plain text.
        if out.is_ascii() {
            out
        } else {
            out.nfkc().collect()
        }
    }

    /// Resolve one code: ToUnicode direct → range rules → byte map.
    /// Borrows table entries; only bfrange hits allocate.
    fn lookup(&self, code: u32, len: usize) -> Option<Cow<'_, str>> {
        if let Some(text) = self.direct.get(&code) {
            return Some(Cow::Borrowed(text));
        }
        for (lo, hi, base, width) in &self.range_rules {
            if *lo <= code && code <= *hi {
                // Wrapping: hostile CMaps set dst bases near u32::MAX.
                let dst = base.wrapping_add(code - lo);
                return Some(Cow::Owned(int_to_utf16(*width, dst)));
            }
        }
        if len == 1 {
            if let Some(text) = &self.byte_map.as_ref()[code as usize] {
                return Some(Cow::Borrowed(text));
            }
        }
        None
    }
}

/// Big-endian bytes → integer.
fn be_int(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
}

/// Integer → UTF-16BE bytes of `width` → lossy string.
fn int_to_utf16(width: usize, value: u32) -> String {
    let mut bytes = vec![0u8; width.max(2)];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let shift = (width.saturating_sub(1 + i)) * 8;
        *slot = ((value >> shift) & 0xFF) as u8;
    }
    utf16be_to_string(&bytes)
}

/// UTF-16BE bytes → string (surrogate pairs supported, lossy).
fn utf16be_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

// ---------------------------------------------------------------------------
// Base encoding tables (built once, shared via `Arc`)
// ---------------------------------------------------------------------------

/// Process-wide shared WinAnsi table.
fn winansi_shared() -> Arc<[Option<String>; 256]> {
    static TABLE: OnceLock<Arc<[Option<String>; 256]>> = OnceLock::new();
    Arc::clone(TABLE.get_or_init(|| Arc::new(winansi_table())))
}

/// Process-wide shared StandardEncoding table.
fn standard_shared() -> Arc<[Option<String>; 256]> {
    static TABLE: OnceLock<Arc<[Option<String>; 256]>> = OnceLock::new();
    Arc::clone(TABLE.get_or_init(|| Arc::new(standard_table())))
}

/// Process-wide shared MacRoman table.
fn macroman_shared() -> Arc<[Option<String>; 256]> {
    static TABLE: OnceLock<Arc<[Option<String>; 256]>> = OnceLock::new();
    Arc::clone(TABLE.get_or_init(|| Arc::new(macroman_table())))
}

/// Full WinAnsiEncoding (CP1252) table.
fn winansi_table() -> [Option<String>; 256] {
    const TABLE: [Option<String>; 256] = [const { None }; 256];
    let mut map = TABLE;
    for byte in 0x20u32..0x7Fu32 {
        map[byte as usize] = Some((byte as u8 as char).to_string());
    }
    map[0x09] = Some("\t".to_string());
    map[0x0A] = Some("\n".to_string());
    map[0x0D] = Some("\r".to_string());
    for byte in 0xA0u32..=0xFFu32 {
        map[byte as usize] = Some((byte as u8 as char).to_string());
    }
    const HIGH: &[(u8, char)] = &[
        (0x80, '€'),
        (0x82, '‚'),
        (0x83, 'ƒ'),
        (0x84, '„'),
        (0x85, '…'),
        (0x86, '†'),
        (0x87, '‡'),
        (0x88, 'ˆ'),
        (0x89, '‰'),
        (0x8A, 'Š'),
        (0x8B, '‹'),
        (0x8C, 'Œ'),
        (0x8E, 'Ž'),
        (0x91, '‘'),
        (0x92, '’'),
        (0x93, '“'),
        (0x94, '”'),
        (0x95, '•'),
        (0x96, '–'),
        (0x97, '—'),
        (0x98, '˜'),
        (0x99, '™'),
        (0x9A, 'š'),
        (0x9B, '›'),
        (0x9C, 'œ'),
        (0x9E, 'ž'),
        (0x9F, 'Ÿ'),
    ];
    for (byte, ch) in HIGH {
        map[*byte as usize] = Some(ch.to_string());
    }
    map
}

/// StandardEncoding: WinAnsi plus the known low-half differences
/// (quoteright/quoteleft). High half approximated as WinAnsi — standard-14
/// fonts without an explicit encoding only ever carry ASCII in practice.
fn standard_table() -> [Option<String>; 256] {
    let mut map = winansi_table();
    map[0x27] = Some('’'.to_string());
    map[0x60] = Some('‘'.to_string());
    map
}

/// MacRomanEncoding: ASCII low half plus the full high-half vector.
fn macroman_table() -> [Option<String>; 256] {
    let mut map = winansi_table();
    // 8 rows × 16 chars, 0x80–0xFF. 0xF0 (Apple logo) has no Unicode mapping.
    const HIGH: &str = concat!(
        "ÄÅÇÉÑÖÜáàâäãåçéè",      // 0x80
        "êëíìîïñóòôöõúùûü",      // 0x90
        "†°¢£§•¶ß®©™´¨≠ÆØ",      // 0xA0
        "∞±≤≥¥µ∂ΣΠπ∫ªºΩæø",      // 0xB0
        "¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ", // 0xC0 (0xCA is U+00A0 nbsp)
        "–—\"\"''÷◊ÿŸ⁄¤‹›ﬁﬂ",    // 0xD0
        "‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ",      // 0xE0
        "�ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ",      // 0xF0 (placeholder, cleared below)
    );
    let chars: Vec<char> = HIGH.chars().collect();
    if chars.len() == 128 {
        for (i, ch) in chars.iter().enumerate() {
            map[0x80 + i] = Some(ch.to_string());
        }
        map[0xF0] = None; // Apple logo: unmappable
    }
    map
}

// ---------------------------------------------------------------------------
// Adobe Glyph List (curated subset + uni/uXXXX parsing)
// ---------------------------------------------------------------------------

/// Resolve a glyph name to Unicode: single ASCII alphanumerics map to
/// themselves, `uniXXXX`/`uXXXX` parse as hex, else the curated AGL table.
fn agl_lookup(name: &str) -> Option<String> {
    if name.len() == 1 {
        let ch = name.chars().next().unwrap_or('?');
        if ch.is_ascii_alphanumeric() {
            return Some(ch.to_string());
        }
    }
    if let Some(hex) = name
        .strip_prefix("uni")
        .filter(|h| h.len() == 4 || h.len() == 8)
        .or_else(|| name.strip_prefix('u').filter(|h| h.len() == 4))
    {
        if let Ok(value) = u32::from_str_radix(hex, 16) {
            if let Some(ch) = char::from_u32(value) {
                return Some(ch.to_string());
            }
        }
    }
    agl_table(name).map(str::to_string)
}

/// Curated AGL subset: punctuation, quotes, dashes, ligatures, TeX extras,
/// western-European accents, common symbols.
fn agl_table(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => " ",
        "nbspace" => "\u{a0}",
        "exclam" => "!",
        "quotedbl" => "\"",
        "numbersign" => "#",
        "dollar" => "$",
        "percent" => "%",
        "ampersand" => "&",
        "quoteright" => "’",
        "parenleft" => "(",
        "parenright" => ")",
        "asterisk" => "*",
        "plus" => "+",
        "comma" => ",",
        "hyphen" => "-",
        "period" => ".",
        "slash" => "/",
        "colon" => ":",
        "semicolon" => ";",
        "less" => "<",
        "equal" => "=",
        "greater" => ">",
        "question" => "?",
        "at" => "@",
        "bracketleft" => "[",
        "backslash" => "\\",
        "bracketright" => "]",
        "asciicircum" => "^",
        "underscore" => "_",
        "quoteleft" => "`",
        "braceleft" => "{",
        "bar" => "|",
        "braceright" => "}",
        "asciitilde" => "~",
        "fi" => "ﬁ",
        "fl" => "ﬂ",
        "ffi" => "ﬃ",
        "ffl" => "ﬄ",
        "dotlessi" => "ı",
        "dotlessj" => "ȷ",
        "quotedblleft" => "“",
        "quotedblright" => "”",
        "quoteleft2" => "‘",
        "quoteright2" => "’",
        "quotesingle" => "‚",
        "quotesinglbase" => "‚",
        "quotedblbase" => "„",
        "endash" => "–",
        "emdash" => "—",
        "bullet" => "•",
        "dagger" => "†",
        "daggerdbl" => "‡",
        "paragraph" => "¶",
        "section" => "§",
        "copyright" => "©",
        "registered" => "®",
        "trademark" => "™",
        "degree" => "°",
        "plusminus" => "±",
        "multiply" => "×",
        "divide" => "÷",
        "logicalnot" => "¬",
        "onehalf" => "½",
        "onequarter" => "¼",
        "threequarters" => "¾",
        "cent" => "¢",
        "sterling" => "£",
        "yen" => "¥",
        "Euro" => "€",
        "florin" => "ƒ",
        "guillemotleft" => "«",
        "guillemotright" => "»",
        "guilsinglleft" => "‹",
        "guilsinglright" => "›",
        "ellipsis" => "…",
        "perthousand" => "‰",
        "minute" => "′",
        "second" => "″",
        "germandbls" => "ß",
        "ae" => "æ",
        "AE" => "Æ",
        "oe" => "œ",
        "OE" => "Œ",
        "oslash" => "ø",
        "Oslash" => "Ø",
        "aring" => "å",
        "Aring" => "Å",
        "thorn" => "þ",
        "Thorn" => "Þ",
        "eth" => "ð",
        "Eth" => "Ð",
        "Lslash" => "Ł",
        "lslash" => "ł",
        "scaron" => "š",
        "Scaron" => "Š",
        "zcaron" => "ž",
        "Zcaron" => "Ž",
        "aacute" => "á",
        "agrave" => "à",
        "acircumflex" => "â",
        "adieresis" => "ä",
        "atilde" => "ã",
        "eacute" => "é",
        "egrave" => "è",
        "ecircumflex" => "ê",
        "edieresis" => "ë",
        "iacute" => "í",
        "igrave" => "ì",
        "icircumflex" => "î",
        "idieresis" => "ï",
        "oacute" => "ó",
        "ograve" => "ò",
        "ocircumflex" => "ô",
        "odieresis" => "ö",
        "otilde" => "õ",
        "uacute" => "ú",
        "ugrave" => "ù",
        "ucircumflex" => "û",
        "udieresis" => "ü",
        "ntilde" => "ñ",
        "yacute" => "ý",
        "ydieresis" => "ÿ",
        "ccedilla" => "ç",
        "Aacute" => "Á",
        "Agrave" => "À",
        "Acircumflex" => "Â",
        "Adieresis" => "Ä",
        "Atilde" => "Ã",
        "Eacute" => "É",
        "Egrave" => "È",
        "Ecircumflex" => "Ê",
        "Edieresis" => "Ë",
        "Iacute" => "Í",
        "Igrave" => "Ì",
        "Icircumflex" => "Î",
        "Idieresis" => "Ï",
        "Oacute" => "Ó",
        "Ograve" => "Ò",
        "Ocircumflex" => "Ô",
        "Odieresis" => "Ö",
        "Otilde" => "Õ",
        "Uacute" => "Ú",
        "Ugrave" => "Ù",
        "Ucircumflex" => "Û",
        "Udieresis" => "Ü",
        "Ntilde" => "Ñ",
        "Ydieresis" => "Ÿ",
        "Ccedilla" => "Ç",
        "softhyphen" => "\u{AD}",
        "radical" => "√",
        "infinity" => "∞",
        "integral" => "∫",
        "partialdiff" => "∂",
        "Delta" => "∆",
        "Omega" => "Ω",
        "omega" => "ω",
        "pi" => "π",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "sigma" => "σ",
        "mu" => "µ",
        "lessequal" => "≤",
        "greaterequal" => "≥",
        "notequal" => "≠",
        "approxequal" => "≈",
        "logicaland" => "∧",
        "logicalor" => "∨",
        "arrowleft" => "←",
        "arrowright" => "→",
        "arrowup" => "↑",
        "arrowdown" => "↓",
        "lozenge" => "◊",
        "diamond" => "♦",
        "heart" => "♥",
        "spade" => "♠",
        "club" => "♣",
        "asteriskmath" => "∗",
        "periodcentered" => "·",
        "diamondmath" => "⋄",
        "minusplus" => "∓",
        "circleplus" => "⊕",
        "circleminus" => "⊖",
        "circlemultiply" => "⊗",
        "circledot" => "⊙",
        "openbullet" => "∘",
        "suchthat" => "∋",
        "therefore" => "∴",
        "congruent" => "≅",
        "propersubset" => "⊂",
        "propersuperset" => "⊃",
        "reflexsubset" => "⊆",
        "reflexsuperset" => "⊇",
        "element" => "∈",
        "universal" => "∀",
        "existential" => "∃",
        "emptyset" => "∅",
        "nabla" => "∇",
        "angle" => "∠",
        "perpendicular" => "⊥",
        "aleph" => "ℵ",
        "weierstrass" => "℘",
        "Ifractur" => "ℑ",
        "Rfractur" => "ℜ",
        "arrowboth" => "↔",
        "arrowdblleft" => "⇐",
        "arrowdblright" => "⇒",
        "arrowdblup" => "⇑",
        "arrowdbldown" => "⇓",
        "arrowdblboth" => "⇔",
        "floorleft" => "⌊",
        "floorright" => "⌋",
        "ceilingleft" => "⌈",
        "ceilingright" => "⌉",
        "dotaccent" => "˙",
        "ring" => "˚",
        "hungarumlaut" => "˝",
        "ogonek" => "˛",
        "caron" => "ˇ",
        "breve" => "˘",
        "macron" => "¯",
        "dieresis" => "¨",
        "acute" => "´",
        "grave" => "`",
        "circumflex" => "ˆ",
        "tilde" => "˜",
        "cedilla" => "¸",
        "alpha" => "α",
        "phi" => "φ",
        "product" => "∏",
        "summation" => "∑",
        "ff" => "ff",
        "brokenbar" => "¦",
        "currency" => "¤",
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        "exclamdown" => "¡",
        "questiondown" => "¿",
        "fraction" => "⁄",
        "onesuperior" => "¹",
        "twosuperior" => "²",
        "threesuperior" => "³",
        "ordfeminine" => "ª",
        "ordmasculine" => "º",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// ToUnicode CMap parser (beginbfchar / beginbfrange)
// ---------------------------------------------------------------------------

/// Parsed ToUnicode CMap.
#[derive(Debug, Clone, Default)]
pub struct ToUnicodeCMap {
    /// Direct code → unicode mappings.
    pub direct: HashMap<u32, String>,
    /// Increment-form ranges: (lo, hi, dst_base, dst_width_bytes).
    pub range_rules: Vec<(u32, u32, u32, usize)>,
    /// Candidate code lengths in bytes, longest first.
    pub code_lengths: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
enum CmapToken {
    Hex(Vec<u8>),
    Literal(Vec<u8>),
    Int(i64),
    Word(String),
}

/// Parse a ToUnicode CMap stream into direct mappings + range rules.
#[must_use]
pub fn parse_tounicode_cmap(data: &[u8]) -> ToUnicodeCMap {
    let tokens = tokenize_cmap(data);
    let mut cmap = ToUnicodeCMap::default();
    let mut lengths: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let count = match &tokens[i] {
            CmapToken::Int(n) => *n.max(&0) as usize,
            _ => {
                i += 1;
                continue;
            }
        };
        let op = tokens.get(i + 1);
        if op != Some(&CmapToken::Word("begincodespacerange".to_string()))
            && op != Some(&CmapToken::Word("beginbfchar".to_string()))
            && op != Some(&CmapToken::Word("beginbfrange".to_string()))
        {
            i += 1;
            continue;
        }
        let op = match op {
            Some(CmapToken::Word(w)) => w.as_str(),
            _ => {
                i += 1;
                continue;
            }
        };
        i += 2;
        for _ in 0..count {
            match op {
                "begincodespacerange" => {
                    if let (Some(CmapToken::Hex(lo)), Some(CmapToken::Hex(hi))) =
                        (tokens.get(i), tokens.get(i + 1))
                    {
                        lengths.push(lo.len().max(1));
                        lengths.push(hi.len().max(1));
                        let _ = (lo, hi);
                    }
                    i += 2;
                }
                "beginbfchar" => {
                    match (tokens.get(i), tokens.get(i + 1)) {
                        (Some(CmapToken::Hex(src)), Some(CmapToken::Hex(dst))) => {
                            cmap.direct.insert(be_int(src), utf16be_to_string(dst));
                        }
                        (Some(CmapToken::Hex(src)), Some(CmapToken::Literal(dst))) => {
                            cmap.direct
                                .insert(be_int(src), String::from_utf8_lossy(dst).into_owned());
                        }
                        _ => {}
                    }
                    i += 2;
                }
                "beginbfrange" => {
                    let (lo, hi) = match (tokens.get(i), tokens.get(i + 1)) {
                        (Some(CmapToken::Hex(lo)), Some(CmapToken::Hex(hi))) => {
                            (be_int(lo), be_int(hi))
                        }
                        _ => {
                            i += 2;
                            continue;
                        }
                    };
                    match tokens.get(i + 2) {
                        Some(CmapToken::Hex(dst)) => {
                            cmap.range_rules
                                .push((lo, hi, be_int(dst), dst.len().max(2)));
                            i += 3;
                        }
                        Some(CmapToken::Word(open)) if open == "[" => {
                            // Array form: lo+k → array[k].
                            let mut k = i + 3;
                            let mut step = 0u32;
                            while k < tokens.len() {
                                match &tokens[k] {
                                    CmapToken::Word(close) if close == "]" => {
                                        k += 1;
                                        break;
                                    }
                                    CmapToken::Hex(dst) => {
                                        // Wrapping: hostile CMaps set lo near u32::MAX.
                                        cmap.direct
                                            .insert(lo.wrapping_add(step), utf16be_to_string(dst));
                                        step += 1;
                                        k += 1;
                                    }
                                    CmapToken::Literal(dst) => {
                                        cmap.direct.insert(
                                            lo.wrapping_add(step),
                                            String::from_utf8_lossy(dst).into_owned(),
                                        );
                                        step += 1;
                                        k += 1;
                                    }
                                    _ => {
                                        k += 1;
                                    }
                                }
                            }
                            let _ = hi;
                            i = k;
                        }
                        Some(CmapToken::Literal(dst)) => {
                            let text = String::from_utf8_lossy(dst).into_owned();
                            for code in lo..=hi.min(lo + 4095) {
                                cmap.direct.insert(code, text.clone());
                            }
                            i += 3;
                        }
                        _ => {
                            i += 3;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    lengths.sort_unstable();
    lengths.dedup();
    lengths.reverse();
    cmap.code_lengths = lengths;
    cmap
}

/// Tokenize CMap content: hex, literals (nested, escaped), names, ints.
fn tokenize_cmap(data: &[u8]) -> Vec<CmapToken> {
    fn is_delim(byte: u8) -> bool {
        byte.is_ascii_whitespace() || b"()<>[]{}/%".contains(&byte)
    }
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let byte = data[i];
        if byte.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if byte == b'%' {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if byte == b'<' && data.get(i + 1) == Some(&b'<') {
            tokens.push(CmapToken::Word("<<".to_string()));
            i += 2;
            continue;
        }
        if byte == b'>' && data.get(i + 1) == Some(&b'>') {
            tokens.push(CmapToken::Word(">>".to_string()));
            i += 2;
            continue;
        }
        if byte == b'<' {
            i += 1;
            let mut hex: Vec<u8> = Vec::new();
            while i < data.len() && data[i] != b'>' {
                if !data[i].is_ascii_whitespace() {
                    hex.push(data[i]);
                }
                i += 1;
            }
            i = (i + 1).min(data.len());
            if hex.len() % 2 == 1 {
                hex.push(b'0');
            }
            let mut bytes = Vec::with_capacity(hex.len() / 2);
            for pair in hex.as_chunks::<2>().0 {
                let text = String::from_utf8_lossy(pair);
                match u8::from_str_radix(&text, 16) {
                    Ok(value) => bytes.push(value),
                    Err(_) => bytes.push(0),
                }
            }
            tokens.push(CmapToken::Hex(bytes));
            continue;
        }
        if byte == b'(' {
            i += 1;
            let mut literal: Vec<u8> = Vec::new();
            let mut depth = 1u32;
            while i < data.len() && depth > 0 {
                match data[i] {
                    b'\\' => {
                        i += 1;
                        if i >= data.len() {
                            break;
                        }
                        match data[i] {
                            b'n' => literal.push(b'\n'),
                            b'r' => literal.push(b'\r'),
                            b't' => literal.push(b'\t'),
                            b'b' => literal.push(8),
                            b'f' => literal.push(12),
                            b'(' => literal.push(b'('),
                            b')' => literal.push(b')'),
                            b'\\' => literal.push(b'\\'),
                            b'\n' => {}
                            b'\r' => {
                                if data.get(i + 1) == Some(&b'\n') {
                                    i += 1;
                                }
                            }
                            digit if digit.is_ascii_digit() && digit < b'8' => {
                                let mut value = u32::from(digit - b'0');
                                for _ in 0..2 {
                                    if i + 1 < data.len()
                                        && data[i + 1].is_ascii_digit()
                                        && data[i + 1] < b'8'
                                    {
                                        i += 1;
                                        value = value * 8 + u32::from(data[i] - b'0');
                                    } else {
                                        break;
                                    }
                                }
                                literal.push(value as u8);
                            }
                            other => literal.push(other),
                        }
                        i += 1;
                    }
                    b'(' => {
                        depth += 1;
                        literal.push(b'(');
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            literal.push(b')');
                        }
                        i += 1;
                    }
                    other => {
                        literal.push(other);
                        i += 1;
                    }
                }
            }
            tokens.push(CmapToken::Literal(literal));
            continue;
        }
        if byte == b'[' || byte == b']' {
            tokens.push(CmapToken::Word(
                String::from_utf8_lossy(&data[i..i + 1]).into_owned(),
            ));
            i += 1;
            continue;
        }
        if byte == b'/' {
            i += 1;
            let start = i;
            while i < data.len() && !is_delim(data[i]) {
                i += 1;
            }
            tokens.push(CmapToken::Word(format!(
                "/{}",
                String::from_utf8_lossy(&data[start..i])
            )));
            continue;
        }
        let start = i;
        while i < data.len() && !is_delim(data[i]) {
            i += 1;
        }
        if start == i {
            // Lone delimiter (>, ), {, }) with no owning construct:
            // skipping it guarantees the loop always makes progress.
            // (Pushing an empty word here used to loop forever, growing
            // the token vec to 4GB+ on garbage CMap bytes.)
            i += 1;
            continue;
        }
        // A byte-consuming tokenizer can never emit more tokens than input
        // bytes; enforce it so any future non-advancing path fails safe.
        if tokens.len() >= data.len() {
            break;
        }
        let word = String::from_utf8_lossy(&data[start..i]).into_owned();
        match word.parse::<i64>() {
            Ok(number) => tokens.push(CmapToken::Int(number)),
            Err(_) => tokens.push(CmapToken::Word(word)),
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfchar_maps_codes() {
        let cmap = parse_tounicode_cmap(b"2 beginbfchar\n<02> <FB01>\n<41> <0041>\nendbfchar");
        assert_eq!(cmap.direct.get(&0x02).map(String::as_str), Some("ﬁ"));
        assert_eq!(cmap.direct.get(&0x41).map(String::as_str), Some("A"));
    }

    #[test]
    fn bfrange_increment_and_array_forms() {
        let cmap = parse_tounicode_cmap(
            b"2 beginbfrange\n<10> <12> <0041>\n<20> <21> [<0043> <0044>]\nendbfrange",
        );
        assert_eq!(cmap.range_rules, vec![(0x10, 0x12, 0x41, 2)]);
        assert_eq!(cmap.direct.get(&0x20).map(String::as_str), Some("C"));
        assert_eq!(cmap.direct.get(&0x21).map(String::as_str), Some("D"));

        let mut decoding = FontDecoding::default();
        decoding.apply_tounicode(&cmap);
        assert_eq!(decoding.decode(&[0x10, 0x11, 0x12]), "ABC");
        assert_eq!(decoding.decode(&[0x20, 0x21]), "CD");
    }

    #[test]
    fn surrogate_pairs_decode() {
        // U+1D400 MATHEMATICAL BOLD CAPITAL A.
        let cmap = parse_tounicode_cmap(b"1 beginbfchar\n<41> <D835DC00>\nendbfchar");
        assert_eq!(
            cmap.direct.get(&0x41).map(String::as_str),
            Some("\u{1D400}")
        );
    }

    #[test]
    fn codespacerange_sets_lengths() {
        let cmap = parse_tounicode_cmap(
            b"2 begincodespacerange\n<00> <80>\n<8100> <9FFF>\nendcodespacerange",
        );
        assert_eq!(cmap.code_lengths, vec![2, 1]);
    }

    #[test]
    fn differences_resolve_via_agl() {
        let mut decoding = FontDecoding::default();
        decoding.apply_differences(&[
            (0x02, "fi".to_string()),
            (0x13, "quotedblleft".to_string()),
            (0x14, "uni20AC".to_string()),
            (0x41, "NotARealGlyphName".to_string()),
        ]);
        // NFKC turns ﬁ (U+FB01) into "fi".
        assert_eq!(decoding.decode(&[0x02]), "fi");
        assert_eq!(decoding.decode(&[0x13]), "\u{201C}");
        assert_eq!(decoding.decode(&[0x14]), "€");
        // Unknown glyph name keeps the base table value.
        assert_eq!(decoding.decode(&[0x41]), "A");
    }

    #[test]
    fn tex_math_names_and_notdef() {
        let mut decoding = FontDecoding::default();
        decoding.apply_differences(&[
            (0x00, ".notdef".to_string()),
            (0x03, "asteriskmath".to_string()),
            (0x0F, "bullet".to_string()),
        ]);
        assert_eq!(decoding.decode(&[0x03]), "\u{2217}");
        assert_eq!(decoding.decode(&[0x0F]), "•");
        assert_eq!(decoding.decode(&[0x00]), "�");
    }

    #[test]
    fn identity_two_byte_cids() {
        let mut decoding = FontDecoding::default();
        decoding.set_two_byte();
        decoding.direct.insert(0x0041, "A".to_string());
        assert_eq!(decoding.decode(&[0x00, 0x41]), "A");
        assert_eq!(decoding.decode(&[0x00]), "�");
    }

    #[test]
    fn winansi_high_bytes_and_controls() {
        let decoding = FontDecoding::default();
        assert_eq!(decoding.decode(&[0x93, 0x94]), "\u{201C}\u{201D}");
        assert_eq!(decoding.decode(b"Hello"), "Hello");
        assert_eq!(decoding.decode(&[0x02]), "�");
    }

    #[test]
    fn macroman_vector_shape() {
        let map = macroman_table();
        assert_eq!(map[0x41], Some("A".to_string()));
        assert_eq!(map[0xDE], Some("ﬁ".to_string()));
        assert_eq!(map[0xF0], None);
    }
}
