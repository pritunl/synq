use std::collections::HashSet;
use std::sync::LazyLock;

static SAFE_CHARS: LazyLock<HashSet<char>> = LazyLock::new(|| {
    HashSet::from([
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm',
        'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M',
        'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
        '-', '+', '=', '_', '/', ',', '.', '~', '@', '#', '!', '&', ' ',
    ])
});

pub fn filter_str(s: &str, n: usize) -> String {
    s.chars()
        .take(n)
        .filter(|c| SAFE_CHARS.contains(c))
        .collect()
}
