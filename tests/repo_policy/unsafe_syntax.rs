use std::fs;
use std::path::{Path, PathBuf};

use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Meta, Path as SynPath, Token};

use super::support::crate_root;

#[derive(Default)]
struct UnsafeSyntax {
    unsafe_nodes: usize,
    exact_unsafe_allows: usize,
    alternate_unsafe_allows: usize,
    cfg_attr_unsafe_allows: usize,
    interop_module_allows: usize,
    denies_unsafe_code: usize,
    forbids_unsafe_code: usize,
}

impl UnsafeSyntax {
    fn parse(source: &str) -> Self {
        let file = syn::parse_file(source).expect("Rust source must parse");
        let mut syntax = Self::default();
        syntax.visit_file(&file);
        syntax
    }

    fn inspect_attribute(&mut self, attribute: &Attribute) {
        self.inspect_meta(&attribute.meta, false);
    }

    fn inspect_meta(&mut self, meta: &Meta, inside_cfg_attr: bool) {
        let Meta::List(list) = meta else {
            return;
        };
        if list.path.is_ident("allow") || list.path.is_ident("expect") {
            let paths = Punctuated::<SynPath, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .expect("allow attribute arguments must parse");
            if paths.iter().any(|path| path.is_ident("unsafe_code")) {
                if inside_cfg_attr {
                    self.cfg_attr_unsafe_allows += 1;
                } else if list.path.is_ident("allow") && paths.len() == 1 {
                    self.exact_unsafe_allows += 1;
                } else {
                    self.alternate_unsafe_allows += 1;
                }
            }
            return;
        }
        if list.path.is_ident("cfg_attr") {
            let nested = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .expect("cfg_attr arguments must parse");
            for meta in nested.iter().skip(1) {
                self.inspect_meta(meta, true);
            }
        }
    }

    fn is_exact_unsafe_allow(attribute: &Attribute) -> bool {
        let Meta::List(list) = &attribute.meta else {
            return false;
        };
        if !list.path.is_ident("allow") {
            return false;
        }
        Punctuated::<SynPath, Token![,]>::parse_terminated
            .parse2(list.tokens.clone())
            .is_ok_and(|paths| paths.len() == 1 && paths[0].is_ident("unsafe_code"))
    }

    fn inspect_crate_lint(&mut self, attribute: &Attribute) {
        let Meta::List(list) = &attribute.meta else {
            return;
        };
        let Ok(paths) =
            Punctuated::<SynPath, Token![,]>::parse_terminated.parse2(list.tokens.clone())
        else {
            return;
        };
        if paths.iter().any(|path| path.is_ident("unsafe_code")) {
            self.denies_unsafe_code += usize::from(list.path.is_ident("deny"));
            self.forbids_unsafe_code += usize::from(list.path.is_ident("forbid"));
        }
    }
}

impl<'ast> Visit<'ast> for UnsafeSyntax {
    fn visit_file(&mut self, file: &'ast syn::File) {
        for attribute in &file.attrs {
            self.inspect_crate_lint(attribute);
        }
        visit::visit_file(self, file);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        self.inspect_attribute(attribute);
        visit::visit_attribute(self, attribute);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast syn::ExprUnsafe) {
        self.unsafe_nodes += 1;
        visit::visit_expr_unsafe(self, expression);
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        self.unsafe_nodes += usize::from(function.sig.unsafety.is_some());
        visit::visit_item_fn(self, function);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if module.ident == "interop" && module.attrs.iter().any(Self::is_exact_unsafe_allow) {
            self.interop_module_allows += 1;
        }
        visit::visit_item_mod(self, module);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        self.unsafe_nodes += usize::from(function.sig.unsafety.is_some());
        visit::visit_impl_item_fn(self, function);
    }

    fn visit_item_impl(&mut self, implementation: &'ast syn::ItemImpl) {
        self.unsafe_nodes += usize::from(implementation.unsafety.is_some());
        visit::visit_item_impl(self, implementation);
    }

    fn visit_item_foreign_mod(&mut self, foreign: &'ast syn::ItemForeignMod) {
        self.unsafe_nodes += usize::from(foreign.unsafety.is_some());
        visit::visit_item_foreign_mod(self, foreign);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        self.unsafe_nodes += usize::from(item.unsafety.is_some());
        visit::visit_item_trait(self, item);
    }

    fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
        self.unsafe_nodes += usize::from(function.sig.unsafety.is_some());
        visit::visit_trait_item_fn(self, function);
    }

    fn visit_foreign_item_fn(&mut self, function: &'ast syn::ForeignItemFn) {
        self.unsafe_nodes += usize::from(function.sig.unsafety.is_some());
        visit::visit_foreign_item_fn(self, function);
    }
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
    {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[test]
fn main_crate_unsafe_syntax_is_confined_to_the_metal_interop_module() {
    let root = crate_root();
    let src = root.join("src");
    let facade = src.join("output/metal.rs");
    let interop = src.join("output/metal/interop.rs");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);

    let mut allowances = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let syntax = UnsafeSyntax::parse(&source);
        assert_eq!(
            syntax.alternate_unsafe_allows,
            0,
            "{} uses a combined or alternate unsafe allowance",
            path.display()
        );
        assert_eq!(
            syntax.cfg_attr_unsafe_allows,
            0,
            "{} can enable unsafe code through cfg_attr",
            path.display()
        );
        if syntax.exact_unsafe_allows != 0 {
            allowances.extend(std::iter::repeat_n(
                path.clone(),
                syntax.exact_unsafe_allows,
            ));
        }
        if path == facade {
            assert_eq!(syntax.interop_module_allows, 1);
            assert_eq!(syntax.denies_unsafe_code, 0);
        } else {
            assert_eq!(syntax.interop_module_allows, 0);
        }
        if path != interop {
            assert_eq!(
                syntax.unsafe_nodes,
                0,
                "{} contains unsafe syntax outside the audited interop module",
                path.display()
            );
        } else {
            assert!(
                syntax.unsafe_nodes > 0,
                "the audited interop module is empty"
            );
        }
    }

    assert_eq!(allowances, vec![facade]);

    let lib = fs::read_to_string(src.join("lib.rs")).expect("read main crate root");
    let lib_syntax = UnsafeSyntax::parse(&lib);
    assert_eq!(lib_syntax.denies_unsafe_code, 1);
    assert_eq!(lib_syntax.forbids_unsafe_code, 0);
}

#[test]
fn unsafe_syntax_parser_handles_formatting_comments_and_attribute_variants() {
    let multiline =
        UnsafeSyntax::parse("unsafe fn f() { unsafe\n{ core::hint::unreachable_unchecked() } }");
    assert_eq!(multiline.unsafe_nodes, 2);

    let comment = UnsafeSyntax::parse("fn safe() { /* unsafe { } */ }");
    assert_eq!(comment.unsafe_nodes, 0);

    let combined = UnsafeSyntax::parse("#[allow(dead_code, unsafe_code)] unsafe fn f() {}");
    assert_eq!(combined.alternate_unsafe_allows, 1);

    let cfg = UnsafeSyntax::parse("#[cfg_attr(any(), allow(unsafe_code))] fn safe() {}");
    assert_eq!(cfg.cfg_attr_unsafe_allows, 1);

    let foreign = UnsafeSyntax::parse("unsafe extern \"C\" { fn f(); }");
    assert_eq!(foreign.unsafe_nodes, 1);

    let implementation = UnsafeSyntax::parse("struct S; unsafe impl Send for S {}");
    assert_eq!(implementation.unsafe_nodes, 1);

    let expected = UnsafeSyntax::parse("#[expect(unsafe_code)] unsafe fn f() {}");
    assert_eq!(expected.alternate_unsafe_allows, 1);

    let unsafe_trait = UnsafeSyntax::parse("unsafe trait T { unsafe fn f(); }");
    assert_eq!(unsafe_trait.unsafe_nodes, 2);
}
