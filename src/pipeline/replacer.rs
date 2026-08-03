use aho_corasick::{AhoCorasick, BuildError, MatchKind};

/// Diccionario de reemplazos compilado una sola vez.
///
/// El automaton de Aho-Corasick se construye al cargar la configuración, no en
/// cada entrega: reconstruirlo por dictado era trabajo puro de arranque
/// repetido en el camino caliente.
#[derive(Debug)]
pub struct Replacer {
    automaton: AhoCorasick,
    replacements: Vec<String>,
}

impl Replacer {
    /// Devuelve `None` si el diccionario está vacío: no hay nada que reemplazar
    /// y así el camino de entrega se saltea el trabajo por completo.
    pub fn build(dictionary: &[(String, String)]) -> Result<Option<Self>, BuildError> {
        if dictionary.is_empty() {
            return Ok(None);
        }

        let patterns: Vec<String> = dictionary.iter().map(|(k, _)| k.to_lowercase()).collect();
        let automaton = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)?;

        Ok(Some(Self {
            automaton,
            replacements: dictionary.iter().map(|(_, v)| v.clone()).collect(),
        }))
    }

    /// Reemplazo case-insensitive en una sola pasada. El texto original se
    /// conserva fuera de las coincidencias: sólo se sustituye lo que matchea.
    pub fn apply(&self, input: &str) -> String {
        let (lower_input, offsets) = lowercase_with_offsets(input);
        let mut result = String::with_capacity(input.len());
        let mut last_end = 0;

        for m in self.automaton.find_iter(&lower_input) {
            // Los offsets del texto en minúsculas se traducen al original: no
            // se puede indexar `input` con ellos porque bajar de caja no
            // preserva la longitud en bytes (`İ` ocupa 2 y su minúscula 3).
            let (start, end) = (offsets[m.start()], offsets[m.end()]);
            if start < last_end {
                continue;
            }
            result.push_str(&input[last_end..start]);
            result.push_str(&self.replacements[m.pattern().as_usize()]);
            last_end = end;
        }
        result.push_str(&input[last_end..]);
        result
    }
}

/// Pasa a minúsculas y devuelve, por cada byte del resultado, el offset del
/// carácter original que lo produjo. El último elemento cierra el rango, para
/// que `offsets[lower.len()]` sea siempre válido.
fn lowercase_with_offsets(input: &str) -> (String, Vec<usize>) {
    let mut lower = String::with_capacity(input.len());
    let mut offsets = Vec::with_capacity(input.len() + 1);

    for (index, character) in input.char_indices() {
        let before = lower.len();
        for lowered in character.to_lowercase() {
            lower.push(lowered);
        }
        // Todo byte producido por este carácter apunta a donde el carácter
        // empieza en el original, así el corte cae siempre en un límite válido.
        offsets.resize(offsets.len() + (lower.len() - before), index);
    }
    offsets.push(input.len());

    (lower, offsets)
}

#[cfg(test)]
mod tests {
    use super::{lowercase_with_offsets, Replacer};

    fn dict(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_dictionary_builds_nothing() {
        assert!(Replacer::build(&[]).unwrap().is_none());
    }

    #[test]
    fn replaces_case_insensitively_preserving_the_rest() {
        let replacer = Replacer::build(&dict(&[("claude", "Claude")]))
            .unwrap()
            .unwrap();
        assert_eq!(replacer.apply("abrí CLAUDE ahora"), "abrí Claude ahora");
    }

    #[test]
    fn longest_pattern_wins_over_its_prefix() {
        let replacer = Replacer::build(&dict(&[("git", "Git"), ("github", "GitHub")]))
            .unwrap()
            .unwrap();
        assert_eq!(replacer.apply("subilo a github"), "subilo a GitHub");
    }

    #[test]
    fn text_without_matches_is_returned_untouched() {
        let replacer = Replacer::build(&dict(&[("xyz", "XYZ")])).unwrap().unwrap();
        assert_eq!(replacer.apply("nada que ver"), "nada que ver");
    }

    #[test]
    fn multibyte_input_does_not_split_characters() {
        let replacer = Replacer::build(&dict(&[("cámara", "camera")]))
            .unwrap()
            .unwrap();
        assert_eq!(replacer.apply("prendé la CÁMARA ñandú"), "prendé la camera ñandú");
    }

    #[test]
    fn offsets_survive_lowercasing_that_grows_the_string() {
        // `İ` (U+0130) ocupa 2 bytes y su minúscula `i̇` ocupa 3: los offsets
        // del texto en minúsculas no sirven para indexar el original.
        let (lower, offsets) = lowercase_with_offsets("İ");
        assert!(lower.len() > "İ".len());
        assert_eq!(offsets.len(), lower.len() + 1);
        assert_eq!(*offsets.last().unwrap(), "İ".len());
    }

    #[test]
    fn match_after_a_growing_character_is_replaced_at_the_right_place() {
        let replacer = Replacer::build(&dict(&[("hola", "chau")])).unwrap().unwrap();
        assert_eq!(replacer.apply("İ hola"), "İ chau");
    }
}
