//! Count policy collections from syntax events before the shared TOML
//! deserializer builds arrays, components, rules or glob strings.
use toml_parser::Source;
use toml_parser::parser::{Event, EventKind, parse_document};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Collection {
    #[default]
    Other,
    Components,
    Rules,
    Paths,
}

pub(super) fn admit(text: &str) -> Result<(), &'static str> {
    let source = Source::new(text);
    // Lex only the already byte-bounded input. Parser events are handled one
    // at a time, without storing an event list or a provisional TOML tree.
    let tokens = source.lex().collect::<Vec<_>>();
    let mut malformed = false;
    let mut counter = Count::default();
    parse_document(
        &tokens,
        &mut |event: Event| counter.event(source, event),
        &mut |_| malformed = true,
    );
    if let Some(reason) = counter.refusal {
        return Err(reason);
    }
    if malformed {
        return Err("INVALID_CONTEXT_CONFIG");
    }
    Ok(())
}

#[derive(Default)]
struct Count {
    components: usize,
    rules: usize,
    globs: usize,
    // Valid syntax reaches at most: components array, component table,
    // paths array. Arrays of tables use a header event instead.
    stack: [Collection; 3],
    depth: usize,
    key: Collection,
    dotted: bool,
    refusal: Option<&'static str>,
}

impl Count {
    fn event(&mut self, source: Source<'_>, event: Event) {
        use EventKind::*;
        if self.refusal.is_some() {
            return;
        }
        match event.kind() {
            SimpleKey if !self.dotted => {
                self.key = collection_key(source, event);
            }
            KeySep => {
                self.key = Collection::Other;
                self.dotted = true;
            }
            ArrayTableOpen | StdTableOpen | StdTableClose => self.clear_key(),
            ArrayTableClose => {
                self.increase(self.key);
                self.clear_key();
            }
            ArrayOpen => self.open_value(self.key),
            InlineTableOpen => self.open_value(Collection::Other),
            ArrayClose | InlineTableClose => {
                self.depth = self.depth.saturating_sub(1);
                self.clear_key();
            }
            Scalar => {
                self.increase(self.parent());
                self.clear_key();
            }
            _ => {}
        }
    }

    fn open_value(&mut self, collection: Collection) {
        self.increase(self.parent());
        self.clear_key();
        if let Some(slot) = self.stack.get_mut(self.depth) {
            *slot = collection;
            self.depth += 1;
        } else if self.refusal.is_none() {
            self.refusal = Some("INVALID_CONTEXT_CONFIG");
        }
    }

    fn clear_key(&mut self) {
        self.key = Collection::Other;
        self.dotted = false;
    }

    fn parent(&self) -> Collection {
        self.depth
            .checked_sub(1)
            .map_or(Collection::Other, |slot| self.stack[slot])
    }

    fn increase(&mut self, collection: Collection) {
        let (count, max) = match collection {
            Collection::Other => return,
            Collection::Components => (&mut self.components, 64),
            Collection::Rules => (&mut self.rules, 256),
            Collection::Paths => (&mut self.globs, 256),
        };
        if *count == max {
            self.refusal = Some("LIMIT_EXCEEDED");
        } else {
            *count += 1;
        }
    }
}

fn collection_key(source: Source<'_>, event: Event) -> Collection {
    // Ten characters in the longest relevant name and at most ten bytes
    // per TOML escape, plus the quotes.
    if event.span().end().saturating_sub(event.span().start()) > 102 {
        return Collection::Other;
    }
    let Some(key) = source.get(event) else {
        return Collection::Other;
    };
    let mut decoded = String::new();
    // The shared parser diagnoses invalid key encoding after the count pass.
    key.decode_key(&mut decoded, &mut |_| {});
    match decoded.as_str() {
        "components" => Collection::Components,
        "rules" => Collection::Rules,
        "paths" => Collection::Paths,
        _ => Collection::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_proposal_caps_array_tables_before_typed_decoding() {
        let components = "[[components]]\nname='a'\npaths=['src/**']\n";
        let rules = "[['rules']]\nid='a'\nfrom='*'\nto='*'\neffect='allow'\n";
        assert_eq!(admit(&components.repeat(64)), Ok(()));
        assert_eq!(admit(&components.repeat(65)), Err("LIMIT_EXCEEDED"));
        assert_eq!(admit(&rules.repeat(256)), Ok(()));
        assert_eq!(admit(&rules.repeat(257)), Err("LIMIT_EXCEEDED"));
    }

    #[test]
    fn architecture_proposal_counts_globs_across_inline_and_header_components() {
        let paths = |count| vec!["'src/**'"; count].join(",");
        let inline = |count| {
            format!(
                "components = [{{name='a', paths=[{}]}}, {{name='b', \"p\\u0061ths\"=[{}]}}]",
                paths(128),
                paths(count),
            )
        };
        let standard = |count| {
            format!(
                "[[components]]\nname='a'\npaths=[{}]\n[[components]]\nname='b'\npaths=[{}]\n",
                paths(128),
                paths(count),
            )
        };
        assert_eq!(admit(&inline(128)), Ok(()));
        assert_eq!(admit(&inline(129)), Err("LIMIT_EXCEEDED"));
        assert_eq!(admit(&standard(128)), Ok(()));
        assert_eq!(admit(&standard(129)), Err("LIMIT_EXCEEDED"));
    }
}
