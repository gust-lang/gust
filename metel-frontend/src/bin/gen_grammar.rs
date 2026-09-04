//! metel-core#720: generates `docs/reference/spec/grammar.md`'s formal-grammar
//! block from `grammar.pest` directly, via `pest_meta` -- the same crate
//! `pest_derive` itself uses to parse `.pest` files, so no hand-written
//! grammar-parsing code is needed here.
//!
//! Every one of `grammar.pest`'s rules needs an entry in the sidecar
//! (`grammar-doc.toml`, next to `grammar.pest`): `mode = "named"` gives it a
//! display name and a doc section (a real, documented production); `mode =
//! "inline"` means the rule is pure syntax sugar (a keyword atom's word-
//! boundary lookahead, e.g.) -- wherever another rule references it, this
//! tool substitutes its own body in place, recursively, so it never gets a
//! line of its own; `mode = "omit"` means the rule is pest-internal
//! plumbing (`WHITESPACE`, `COMMENT`) that no *documented* rule should ever
//! reference -- if one somehow does, that is treated as a sidecar mistake
//! (fix it to `inline`), not silently rendered.
//!
//! Usage (run from metel-frontend/):
//!   cargo run --bin gen_grammar [-- --check]
//!     (no args)  regenerate docs/reference/spec/grammar.md in place
//!     --check    exit 1 if the checked-in file doesn't match a fresh
//!                generation, without writing anything (CI drift check)

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use pest_meta::ast::Expr;
use pest_meta::parser;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum SidecarMode {
    /// A real, documented production: gets its own line under `section`,
    /// rendered from the rule's actual pest body.
    Named { name: String, section: String },
    /// An opaque named token (IDENTIFIER, INT, STRING, ...): gets a display
    /// name used wherever referenced, but never a line of its own -- its
    /// internal structure (a character-class regex, typically) is exactly
    /// the kind of detail a grammar reader doesn't want spelled out.
    Terminal { name: String },
    /// Pure indirection or trivial wrapping (`bang = { "!" }`, a
    /// single-alternative redirect): substitutes the rule's own pest body
    /// in place, recursively, wherever referenced. Never gets a line.
    Inline,
    /// Like `inline`, but the substituted text is an explicit override
    /// instead of the rule's real pest body -- for the keyword atoms whose
    /// actual body is `"kw" ~ !(ASCII_ALPHANUMERIC | "_")`: the trailing
    /// word-boundary lookahead is a lexing necessity, not grammar
    /// information a reader needs, and stripping it mechanically (pattern-
    /// matching the lookahead shape) risks silently mishandling a rule that
    /// isn't actually that shape. An explicit override says exactly what
    /// renders, still checked at the same completeness/no-omitted-refs
    /// gates as every other mode. `values` joins as `"a" | "b" | ...`
    /// (a keyword atom has one; `bool_lit`-shaped rules have several).
    Literal { values: Vec<String> },
    /// Pest-internal plumbing (`WHITESPACE`, `COMMENT`, an interpolation
    /// sub-scanner): must never actually be reached while rendering a
    /// documented rule's body. Reaching one is treated as a sidecar
    /// mistake (see `check_no_omitted_refs`), not silently rendered.
    Omit,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SidecarEntry {
    pest: String,
    #[serde(flatten)]
    mode: SidecarMode,
}

#[derive(Debug, serde::Deserialize)]
struct Sidecar {
    #[serde(rename = "rule")]
    rules: Vec<SidecarEntry>,
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is metel-frontend/ -- the workspace root (metel-core
    // checkout root, docs/ submodule mounted under it) is one level up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("metel-frontend has a parent directory")
        .to_path_buf()
}

fn grammar_pest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/grammar.pest")
}

fn sidecar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/grammar-doc.toml")
}

fn grammar_md_path() -> PathBuf {
    workspace_root().join("docs/reference/spec/grammar.md")
}

fn main() -> ExitCode {
    let check_only = std::env::args().any(|a| a == "--check");

    let grammar_text = std::fs::read_to_string(grammar_pest_path())
        .unwrap_or_else(|e| panic!("reading grammar.pest: {e}"));
    let sidecar_text = std::fs::read_to_string(sidecar_path())
        .unwrap_or_else(|e| panic!("reading grammar-doc.toml: {e}"));
    let sidecar: Sidecar =
        toml::from_str(&sidecar_text).unwrap_or_else(|e| panic!("parsing grammar-doc.toml: {e}"));

    let pairs = parser::parse(parser::Rule::grammar_rules, &grammar_text)
        .unwrap_or_else(|e| panic!("parsing grammar.pest:\n{e}"));
    let rules = parser::consume_rules(pairs)
        .unwrap_or_else(|e| panic!("building grammar.pest AST:\n{:?}", e));

    let by_name: HashMap<String, Expr> = rules
        .iter()
        .map(|r| (r.name.clone(), r.expr.clone()))
        .collect();

    // Completeness check: every real pest rule needs a sidecar entry, and
    // every sidecar entry needs to name a real pest rule -- each direction
    // catches a different mistake (a new rule nobody documented; a stale
    // entry for a rule that got renamed or deleted).
    let mut sidecar_by_pest: HashMap<String, SidecarMode> = HashMap::new();
    for entry in &sidecar.rules {
        if sidecar_by_pest
            .insert(entry.pest.clone(), entry.mode.clone())
            .is_some()
        {
            panic!(
                "grammar-doc.toml: duplicate entry for pest rule `{}`",
                entry.pest
            );
        }
    }
    let mut problems = Vec::new();
    for rule in &rules {
        if !sidecar_by_pest.contains_key(&rule.name) {
            problems.push(format!(
                "grammar.pest rule `{}` has no grammar-doc.toml entry",
                rule.name
            ));
        }
    }
    for entry in &sidecar.rules {
        if !by_name.contains_key(&entry.pest) {
            problems.push(format!(
                "grammar-doc.toml entry `{}` names a pest rule that doesn't exist",
                entry.pest
            ));
        }
    }
    if !problems.is_empty() {
        eprintln!("gen_grammar: sidecar/grammar mismatch:");
        for p in &problems {
            eprintln!("  - {p}");
        }
        return ExitCode::FAILURE;
    }

    // Section order = the order `section` values first appear among `named`
    // entries in the sidecar array -- the sidecar file's own ordering is the
    // only place doc structure is decided, so there is exactly one thing to
    // edit to reorder or regroup the document.
    let mut section_order: Vec<String> = Vec::new();
    let mut sections: BTreeMap<String, Vec<&SidecarEntry>> = BTreeMap::new();
    for entry in &sidecar.rules {
        if let SidecarMode::Named { section, .. } = &entry.mode {
            if !sections.contains_key(section.as_str()) {
                section_order.push(section.clone());
            }
            sections.entry(section.clone()).or_default().push(entry);
        }
    }

    let renderer = Renderer {
        by_name: &by_name,
        sidecar: &sidecar_by_pest,
    };

    // Referenced-while-omitted check: an `omit` rule must never actually be
    // reached while rendering a named rule's body -- if it is, the sidecar
    // classified it wrong (should be `inline`), and silently rendering it
    // as if inline would hide that mistake instead of catching it.
    for entry in &sidecar.rules {
        if let SidecarMode::Named { .. } = &entry.mode {
            renderer.check_no_omitted_refs(&by_name[&entry.pest], &entry.pest);
        }
    }

    let mut out = String::new();
    out.push_str("# Grammar\n\n");
    out.push_str("<!-- Generated by metel-frontend/src/bin/gen_grammar.rs from grammar.pest\n");
    out.push_str("     and grammar-doc.toml -- do not hand-edit. Run\n");
    out.push_str("     `cargo run --bin gen_grammar` from metel-frontend/ to regenerate. -->\n\n");
    out.push_str("```\n");
    let mut first_section = true;
    for section in &section_order {
        if !first_section {
            out.push('\n');
        }
        first_section = false;
        let entries = &sections[section];
        let name_width = entries
            .iter()
            .map(|e| match &e.mode {
                SidecarMode::Named { name, .. } => name.chars().count(),
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        for entry in entries {
            let SidecarMode::Named { name, .. } = &entry.mode else {
                unreachable!("sections only ever collects Named entries")
            };
            let alts = renderer.render_top(&by_name[&entry.pest]);
            let pad = " ".repeat(name_width.saturating_sub(name.chars().count()));
            let arrow_col = name_width + 1;
            for (i, line) in alts.iter().enumerate() {
                if i == 0 {
                    writeln!(out, "{name}{pad} → {line}").unwrap();
                } else {
                    writeln!(out, "{}| {line}", " ".repeat(arrow_col)).unwrap();
                }
            }
        }
    }
    out.push_str("```\n");

    if check_only {
        let existing = std::fs::read_to_string(grammar_md_path()).unwrap_or_default();
        if existing == out {
            println!("gen_grammar --check: grammar.md is current.");
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "gen_grammar --check: docs/reference/spec/grammar.md is stale -- \
                 run `cargo run --bin gen_grammar` (from metel-frontend/) to regenerate."
            );
            ExitCode::FAILURE
        }
    } else {
        std::fs::write(grammar_md_path(), &out)
            .unwrap_or_else(|e| panic!("writing grammar.md: {e}"));
        println!("gen_grammar: wrote {}", grammar_md_path().display());
        ExitCode::SUCCESS
    }
}

struct Renderer<'a> {
    by_name: &'a HashMap<String, Expr>,
    sidecar: &'a HashMap<String, SidecarMode>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Ctx {
    /// The top-level RHS of a named rule -- a `|` chain here needs no parens.
    Top,
    /// One element of a `~` sequence, or the branch of a `|` -- an inner `|`
    /// (when not itself the whole thing) needs parens; an inner `~` never
    /// does (sequencing is left-associative-looking either way).
    Inner,
    /// The operand of a unary postfix (`?`/`*`/`+`/`{..}`) or prefix
    /// (`&`/`!`) operator -- anything but a bare Str/Ident/Range needs
    /// parens.
    Atom,
}

impl<'a> Renderer<'a> {
    /// `values` folded right-associatively into a `Choice` chain (or a bare
    /// `Str` for one value) -- the same shape `resolve_fully` already knows
    /// how to flatten, so a `literal` entry needs no separate code path
    /// anywhere downstream of this.
    fn literal_expr(values: &[String]) -> Expr {
        match values {
            [] => panic!("grammar-doc.toml: a `literal` entry needs at least one value"),
            [v] => Expr::Str(v.clone()),
            [v, rest @ ..] => Expr::Choice(
                Box::new(Expr::Str(v.clone())),
                Box::new(Self::literal_expr(rest)),
            ),
        }
    }

    /// One level of inline-substitution: an `Ident` naming an `inline` or
    /// `literal` rule expands to that rule's own body / override; anything
    /// else -- including an `Ident` naming a `named`/`terminal` rule, which
    /// stays a reference -- passes through as a clone unchanged. Never
    /// recurses on its own; callers that need the fully-resolved shape call
    /// this in a loop / recursively themselves, so each call site controls
    /// exactly how much unfolding it wants at that point.
    fn resolve_one(&self, expr: &Expr) -> Expr {
        if let Expr::Ident(id) = expr {
            match self.sidecar.get(id) {
                Some(SidecarMode::Inline) => return self.by_name[id].clone(),
                Some(SidecarMode::Literal { values }) => return Self::literal_expr(values),
                _ => {}
            }
        }
        expr.clone()
    }

    /// Fully resolves inline/literal chains until the top node is no longer
    /// an inline- or literal-rule reference (an inline rule can itself
    /// reference another inline or literal rule).
    fn resolve_fully(&self, expr: &Expr) -> Expr {
        let mut current = expr.clone();
        loop {
            let should_continue = matches!(
                &current,
                Expr::Ident(id)
                    if matches!(
                        self.sidecar.get(id),
                        Some(SidecarMode::Inline) | Some(SidecarMode::Literal { .. })
                    )
            );
            let next = self.resolve_one(&current);
            if !should_continue {
                return next;
            }
            current = next;
        }
    }

    fn check_no_omitted_refs(&self, expr: &Expr, in_rule: &str) {
        if let Expr::Ident(id) = expr {
            match self.sidecar.get(id) {
                Some(SidecarMode::Omit) => panic!(
                    "grammar-doc.toml: `{id}` is `omit` but is referenced from `{in_rule}` \
                     (transitively) -- change its mode to `inline` if it should render, \
                     or fix `{in_rule}` if the reference is a mistake"
                ),
                Some(SidecarMode::Inline) => {
                    self.check_no_omitted_refs(&self.by_name[id], id);
                }
                // `literal`'s substituted text is a hand-written override,
                // not derived from the rule's own body -- nothing to
                // recurse into. `terminal` is an opacity boundary by
                // design (its body is never rendered), same reasoning.
                Some(SidecarMode::Literal { .. })
                | Some(SidecarMode::Terminal { .. })
                | Some(SidecarMode::Named { .. })
                | None => {}
            }
            return;
        }
        for child in expr.iter_top_down().skip(1) {
            self.check_no_omitted_refs(&child, in_rule);
        }
    }

    /// Renders `expr` as the top-level RHS of a named rule: flattens a
    /// top-level `Choice` (after resolving inlines) into one alternative
    /// per line.
    fn render_top(&self, expr: &Expr) -> Vec<String> {
        let mut alts = Vec::new();
        self.collect_choice_alts(expr, &mut alts);
        alts.iter().map(|e| self.render(e, Ctx::Top)).collect()
    }

    fn collect_choice_alts(&self, expr: &Expr, out: &mut Vec<Expr>) {
        match self.resolve_fully(expr) {
            Expr::Choice(lhs, rhs) => {
                self.collect_choice_alts(&lhs, out);
                self.collect_choice_alts(&rhs, out);
            }
            other => out.push(other),
        }
    }

    fn collect_seq(&self, expr: &Expr, out: &mut Vec<Expr>) {
        match self.resolve_fully(expr) {
            Expr::Seq(lhs, rhs) => {
                self.collect_seq(&lhs, out);
                self.collect_seq(&rhs, out);
            }
            // pest's start-of-input marker, referenced directly from
            // `program`'s own body (`SOI ~ ...`) -- a pseudo-rule with no
            // entry in `parser::consume_rules`'s output at all (it's a pest
            // built-in, not a user-defined rule needing a sidecar
            // classification), and one every grammar implicitly starts at,
            // so it carries no display information of its own. Dropped
            // here rather than rendered-then-filtered, so an empty string
            // never has to survive a `" ".join(...)` downstream.
            Expr::Ident(id) if id == "SOI" => {}
            other if is_word_boundary_guard(&other) => {}
            other => out.push(other),
        }
    }

    fn render(&self, expr: &Expr, ctx: Ctx) -> String {
        let resolved = self.resolve_fully(expr);
        match resolved {
            Expr::Str(s) => format!("{:?}", s),
            Expr::Insens(s) => format!("^{:?}", s),
            Expr::Range(a, b) => format!("{:?}..{:?}", a, b),
            // pest's end-of-input marker -- same pseudo-rule status as SOI
            // above, but (unlike SOI) genuinely meaningful to show: `EOF`
            // matches the old hand-written grammar.md's own convention.
            Expr::Ident(id) if id == "EOI" => "\"EOF\"".to_string(),
            Expr::Ident(id) => match self.sidecar.get(&id) {
                Some(SidecarMode::Named { name, .. } | SidecarMode::Terminal { name }) => {
                    name.clone()
                }
                other => unreachable!(
                    "`{id}` ({other:?}): inline/literal/omit idents are resolved before rendering"
                ),
            },
            Expr::PosPred(e) => format!("&{}", self.render(&e, Ctx::Atom)),
            Expr::NegPred(e) => format!("!{}", self.render(&e, Ctx::Atom)),
            Expr::Seq(_, _) => {
                let mut parts = Vec::new();
                self.collect_seq(&resolved, &mut parts);
                let rendered = parts
                    .iter()
                    .map(|e| self.render(e, Ctx::Inner))
                    .collect::<Vec<_>>()
                    .join(" ");
                if ctx == Ctx::Atom {
                    format!("( {rendered} )")
                } else {
                    rendered
                }
            }
            Expr::Choice(_, _) => {
                let mut parts = Vec::new();
                self.collect_choice_alts(&resolved, &mut parts);
                let rendered = parts
                    .iter()
                    .map(|e| self.render(e, Ctx::Inner))
                    .collect::<Vec<_>>()
                    .join(" | ");
                if ctx == Ctx::Top {
                    rendered
                } else {
                    format!("( {rendered} )")
                }
            }
            Expr::Opt(e) => format!("{}?", self.render(&e, Ctx::Atom)),
            Expr::Rep(e) => format!("{}*", self.render(&e, Ctx::Atom)),
            Expr::RepOnce(e) => format!("{}+", self.render(&e, Ctx::Atom)),
            Expr::RepExact(e, n) => format!("{}{{{}}}", self.render(&e, Ctx::Atom), n),
            Expr::RepMin(e, n) => format!("{}{{{},}}", self.render(&e, Ctx::Atom), n),
            Expr::RepMax(e, n) => format!("{}{{,{}}}", self.render(&e, Ctx::Atom), n),
            Expr::RepMinMax(e, min, max) => {
                format!("{}{{{},{}}}", self.render(&e, Ctx::Atom), min, max)
            }
            other => panic!(
                "gen_grammar: encountered a pest expression ({other:?}) the renderer has no \
                 documented notation for yet -- add one rather than silently rendering nothing"
            ),
        }
    }
}

/// Recognizes exactly `!(ASCII_ALPHANUMERIC | "_")` (in any grouping pest's
/// own parser happens to produce) -- the word-boundary lookahead
/// grammar.pest's own header comment names as its standing convention for
/// every keyword atom, spelled out ad hoc inline for the one case that
/// isn't its own separate rule (`pattern`'s wildcard alternative, `"_" ~
/// !(ASCII_ALPHANUMERIC | "_")`, guarding against matching `_foo`). Every
/// *rule-level* instance of this idiom already has an explicit `literal`
/// sidecar override (grammar-doc.toml); this catches the one embedded
/// instance no sidecar entry could ever attach to, matching by exact shape
/// rather than guessing at a wider pattern -- deliberately narrow so a
/// grammar.pest change that adds some *other* lookahead doesn't silently
/// vanish through this instead of hitting the "no notation for this yet"
/// panic above.
fn is_word_boundary_guard(expr: &Expr) -> bool {
    fn is_alnum_or_underscore(expr: &Expr) -> bool {
        match expr {
            Expr::Ident(id) if id == "ASCII_ALPHANUMERIC" => true,
            Expr::Str(s) if s == "_" => true,
            Expr::Choice(lhs, rhs) => is_alnum_or_underscore(lhs) && is_alnum_or_underscore(rhs),
            _ => false,
        }
    }
    matches!(expr, Expr::NegPred(inner) if is_alnum_or_underscore(inner))
}
