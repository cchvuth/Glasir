use std::collections::BTreeSet;
use std::path::Path;
use syn::visit::Visit;

struct V<'a> {
    module: String,
    file: &'a str,
    types: Vec<String>,
    trait_impl: Vec<bool>,
    test: Vec<bool>,
    cur: Vec<(String, BTreeSet<String>)>,
    out_fns: &'a mut Vec<String>,
    out_calls: &'a mut Vec<String>,
}

fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let p = a.path();
        if p.is_ident("test") {
            return true;
        }
        if p.is_ident("cfg") {
            let s = a.meta.to_token_stream_string();
            return s.contains("test") && !s.contains("not");
        }
        false
    })
}

trait TS {
    fn to_token_stream_string(&self) -> String;
}
impl TS for syn::Meta {
    fn to_token_stream_string(&self) -> String {
        quote_like(self)
    }
}
fn quote_like(m: &syn::Meta) -> String {
    match m {
        syn::Meta::List(l) => l.tokens.to_string(),
        syn::Meta::Path(p) => p.segments.iter().map(|s| s.ident.to_string()).collect::<Vec<_>>().join("::"),
        syn::Meta::NameValue(_) => String::new(),
    }
}

fn idents(ts: proc_macro2::TokenStream, set: &mut BTreeSet<String>) {
    for t in ts {
        match t {
            proc_macro2::TokenTree::Ident(i) => {
                set.insert(i.to_string());
            }
            proc_macro2::TokenTree::Group(g) => idents(g.stream(), set),
            _ => {}
        }
    }
}

impl<'a> V<'a> {
    fn testing(&self) -> bool {
        self.test.last().copied().unwrap_or(false)
    }
    fn enter_fn(&mut self, name: &str, attrs: &[syn::Attribute], span: proc_macro2::Span, end: usize) {
        let mut id = self.module.clone();
        if let Some((outer, _)) = self.cur.last() {
            id = outer.clone();
        } else if let Some(t) = self.types.last() {
            id = format!("{id}::{t}");
        }
        id = format!("{id}::{name}");
        let test = self.testing() || is_test(attrs);
        let ti = self.trait_impl.last().copied().unwrap_or(false);
        let ty = if self.cur.is_empty() { self.types.last().cloned().unwrap_or_else(|| "-".into()) } else { "-".into() };
        self.out_fns.push(format!("{id}\t{}\t{}\t{}\t{}\t{}\t{name}\t{ty}\t{}", self.file, span.start().line, end, test as u8, ti as u8, self.module));
        self.test.push(test);
        self.cur.push((id, BTreeSet::new()));
    }
    fn leave_fn(&mut self) {
        self.test.pop();
        if let Some((id, set)) = self.cur.pop() {
            self.out_calls.push(format!("{id}\t{}", set.into_iter().collect::<Vec<_>>().join(",")));
        }
    }
    fn note(&mut self, name: String) {
        if let Some((_, set)) = self.cur.last_mut() {
            set.insert(name);
        } else {
            let id = format!("{}::<items>", self.module);
            self.out_calls.push(format!("{id}\t{name}"));
        }
    }
}

fn type_name(t: &syn::Type) -> String {
    match t {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
        syn::Type::Reference(r) => type_name(&r.elem),
        _ => "?".into(),
    }
}

impl<'ast, 'a> Visit<'ast> for V<'a> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let t = self.testing() || is_test(&m.attrs);
        let saved = self.module.clone();
        self.module = format!("{}::{}", self.module, m.ident);
        self.test.push(t);
        syn::visit::visit_item_mod(self, m);
        self.test.pop();
        self.module = saved;
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        self.types.push(type_name(&i.self_ty));
        self.trait_impl.push(i.trait_.is_some());
        let t = self.testing() || is_test(&i.attrs);
        self.test.push(t);
        syn::visit::visit_item_impl(self, i);
        self.test.pop();
        self.trait_impl.pop();
        self.types.pop();
    }
    fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
        self.types.push(i.ident.to_string());
        self.trait_impl.push(true);
        syn::visit::visit_item_trait(self, i);
        self.trait_impl.pop();
        self.types.pop();
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.enter_fn(&f.sig.ident.to_string(), &f.attrs, f.sig.ident.span(), f.block.brace_token.span.close().end().line);
        syn::visit::visit_item_fn(self, f);
        self.leave_fn();
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.enter_fn(&f.sig.ident.to_string(), &f.attrs, f.sig.ident.span(), f.block.brace_token.span.close().end().line);
        syn::visit::visit_impl_item_fn(self, f);
        self.leave_fn();
    }
    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if f.default.is_some() {
            let end = f.default.as_ref().map_or(0, |b| b.brace_token.span.close().end().line);
            self.enter_fn(&f.sig.ident.to_string(), &f.attrs, f.sig.ident.span(), end);
            syn::visit::visit_trait_item_fn(self, f);
            self.leave_fn();
        }
    }
    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let segs: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
        self.note(format!("p:{}", segs.join("::")));
        syn::visit::visit_expr_path(self, p);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        self.note(format!("m:{}", m.method));
        syn::visit::visit_expr_method_call(self, m);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let mut set = BTreeSet::new();
        idents(m.tokens.clone(), &mut set);
        for s in set {
            self.note(format!("x:{s}"));
        }
        syn::visit::visit_macro(self, m);
    }
}

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, files);
        } else if p.extension().is_some_and(|x| x == "rs") {
            files.push(p);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("src dir");
    let out = std::env::args().nth(2).expect("out dir");
    let root = Path::new(&root);
    let mut files = vec![];
    walk(root, &mut files);
    files.sort();
    let (mut fns, mut calls) = (vec![], vec![]);
    for f in &files {
        let rel = f.strip_prefix(root.parent().unwrap()).unwrap().to_string_lossy().into_owned();
        let m = f.strip_prefix(root).unwrap().with_extension("");
        let mut parts: Vec<String> = m.iter().map(|c| c.to_string_lossy().into_owned()).collect();
        if parts.last().is_some_and(|l| l == "mod" || l == "main") {
            parts.pop();
        }
        let module = if parts.is_empty() { "crate".to_owned() } else { format!("crate::{}", parts.join("::")) };
        let src = std::fs::read_to_string(f).unwrap();
        let ast = syn::parse_file(&src).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let mut v = V { module, file: &rel, types: vec![], trait_impl: vec![], test: vec![], cur: vec![], out_fns: &mut fns, out_calls: &mut calls };
        v.visit_file(&ast);
    }
    std::fs::write(Path::new(&out).join("fns.tsv"), fns.join("\n") + "\n").unwrap();
    std::fs::write(Path::new(&out).join("calls.tsv"), calls.join("\n") + "\n").unwrap();
    eprintln!("{} files, {} fns", files.len(), fns.len());
}
