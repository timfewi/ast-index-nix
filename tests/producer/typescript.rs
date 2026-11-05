//! Real compiler counterexamples, authored independently of the validator.
use ast_index::{Engine, model::Reference, producer, query::Query};
use std::fs;

const CONFIG: &str = r#"{"compilerOptions":{"strict":true,"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","jsx":"preserve"},"include":["*.ts","*.tsx"]}"#;
const OPS: &str = "export class Base { get() {return 1;} static getStatic() {return 1;} }\nexport function target() {return 1;}\nexport function replacement() {return 2;}\n";

#[test]
fn added_typescript_inputs_invalidate_the_complete_semantic_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("tsconfig.json"), CONFIG).unwrap();
    fs::write(
        root.join("main.ts"),
        "export function target() {} export function caller() {target();}",
    )
    .unwrap();
    let state = directory.path().join("state");
    producer::typescript(&root, &state, &Default::default()).unwrap();
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let query = Query {
        action: "callees".into(),
        query: Some("caller".into()),
        evidence: Some("combined".into()),
        ..Default::default()
    };
    assert_eq!(
        engine.execute(&query).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(root.join("added.ts"), "export const added = 1;").unwrap();
    assert!(
        engine.execute(&query).is_err(),
        "new selected input must invalidate the old snapshot"
    );
}

#[test]
fn real_typescript_producer_abstains_for_mutation_escape_and_structural_receivers() {
    // A separate project per counterexample prevents one broad abstention from
    // concealing another unsafe case. Most also require an unrelated positive.
    let cases = [
        (
            "typed receiver",
            "export function suspect(value: Base) { return value./*deny*/get(); }",
            false,
        ),
        (
            "mutable callback",
            "export function suspect() { let callback = target; return /*deny*/callback(); }",
            false,
        ),
        (
            "reassigned function binding",
            "let local = () => 1; export function suspect() {local = replacement; return /*deny*/local();}",
            false,
        ),
        (
            "overwritten method",
            "export function suspect() {const value = new Base(); value.get = replacement; return value./*deny*/get();}",
            false,
        ),
        (
            "escaped receiver",
            "function replace(value: Base) {Object.defineProperty(value, 'get', {value: replacement});} export function suspect() {const value = new Base(); replace(value); return value./*deny*/get();}",
            false,
        ),
        (
            "captured receiver",
            "export function suspect() {const value = new Base(); return () => value./*deny*/get();}",
            false,
        ),
        (
            "aliased class",
            "function replace(value: typeof Base) {Object.defineProperty(value, 'getStatic', {value: replacement});} const Alias = Base; replace(Alias); export function suspect() {return Base./*deny*/getStatic();}",
            false,
        ),
        (
            "shadowed namespace",
            "export function suspect(ops: typeof import('./ops')) {return ops./*deny*/target();}",
            false,
        ),
        (
            "shadowed namespace constructor",
            "export function suspect(ops: typeof import('./ops')) {const value = new ops.Base(); return value./*deny*/get();}",
            false,
        ),
        (
            "structural namespace",
            "const lookalike: typeof import('./ops') = {Base, target: replacement, replacement}; export function suspect() {return lookalike./*deny*/target();}",
            false,
        ),
        (
            "decorated class",
            "function replaceClass(value: any, context: any): any {return class {get() {return 2;}};} @replaceClass class Decorated {get() {return 1;}} export function suspect() {const value = new Decorated(); return value./*deny*/get();}",
            false,
        ),
        (
            "decorated method",
            "function replaceMethod(value: any, context: any): any {return replacement;} class Decorated {@replaceMethod get() {return 1;}} export function suspect() {const value = new Decorated(); return value./*deny*/get();}",
            false,
        ),
        (
            "decorated field",
            "function field(value: unknown, context: any) {return function(this: any, initial: number) {Object.defineProperty(this, 'get', {value: replacement}); return initial;};} class DecoratedField {@field value = 1; get() {return 1;}} export function suspect() {const value = new DecoratedField(); return value./*deny*/get();}",
            false,
        ),
        (
            "constructor parameter effect",
            "function change(value: object) {Object.defineProperty(value, 'get', {value: replacement}); return 0;} class DefaultEffect {constructor(unused = change(this)) {} get() {return 1;}} export function suspect() {const value = new DefaultEffect(); return value./*deny*/get();}",
            false,
        ),
        (
            "constructor replacement",
            "class ProxyClass {constructor() {return new Proxy(this, {});} get() {return 1;}} export function suspect() {const value = new ProxyClass(); return value./*deny*/get();}",
            false,
        ),
        (
            "escaping this",
            "let escaped: unknown; class Escaping {constructor() {escaped = this;} get() {return 1;}} export function suspect() {const value = new Escaping(); return value./*deny*/get();}",
            false,
        ),
        (
            "accessor",
            "class Accessor {get action() {return replacement;}} export function suspect() {const value = new Accessor(); return value./*deny*/action();}",
            false,
        ),
        (
            "prototype mutation",
            "Object.defineProperty(Base.prototype, 'get', {value: replacement}); export function suspect() {const value = new Base(); return value./*deny*/get();}",
            false,
        ),
        (
            "inheritance escape",
            "class Derived extends Base {static {Object.defineProperty(Object.getPrototypeOf(this), 'getStatic', {value: replacement});}} export function suspect() {return Base./*deny*/getStatic();}",
            false,
        ),
        (
            "generic inheritance escape",
            "class Generic<T> {static getStatic() {return 1;}} class Derived extends Generic<number> {static {Object.defineProperty(Object.getPrototypeOf(this), 'getStatic', {value: replacement});}} export function suspect() {return Generic./*deny*/getStatic();}",
            false,
        ),
        (
            "computed mutation",
            "export function suspect() {const value = new Base(); value['get'] = replacement; return value./*deny*/get();}",
            true,
        ),
        (
            "eval mutation",
            "export function suspect() {const value = new Base(); eval('value.get = replacement'); return value./*deny*/get();}",
            true,
        ),
    ];
    for (name, suspect, dynamic) in cases {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("tsconfig.json"), CONFIG).unwrap();
        fs::write(root.join("ops.ts"), OPS).unwrap();
        let source = format!(
            "import {{Base, target, replacement}} from './ops';\nimport * as ops from './ops';\nfunction sentinel() {{return 1;}}\nexport function control() {{return /*allow*/sentinel();}}\n{suspect}\n"
        );
        fs::write(root.join("main.ts"), &source).unwrap();
        let state = directory.path().join("state");
        producer::typescript(&root, &state, &Default::default())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let engine = Engine::open_state_dir(&root, &state).unwrap();
        let result = engine
            .execute(&Query {
                action: "refs".into(),
                kind: Some("call".into()),
                file: Some("main.ts".into()),
                evidence: Some("combined".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        let references: Vec<Reference> = serde_json::from_value(result["items"].clone()).unwrap();
        for (marker, resolved) in [("/*deny*/", false), ("/*allow*/", !dynamic)] {
            let offset = source.find(marker).unwrap() + marker.len();
            let prefix = &source[..offset];
            let row = prefix.bytes().filter(|b| *b == b'\n').count() as u32;
            let column = prefix.rsplit('\n').next().unwrap().len() as u32;
            let matching: Vec<_> = references
                .iter()
                .filter(|r| r.range.is_some_and(|range| range[..2] == [row, column]))
                .collect();
            assert_eq!(matching.len(), 1, "{name}, {marker}: {result}");
            assert_eq!(
                matching[0].resolved.is_some(),
                resolved,
                "{name}, {marker}: {:?}",
                matching[0]
            );
        }
    }
}

#[test]
fn real_tsx_producer_preserves_language_identity_and_exact_calls() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("tsconfig.json"), CONFIG).unwrap();
    fs::write(
        root.join("main.tsx"),
        "export function target() {return <></>;}\nexport function caller() {return target();}\n",
    )
    .unwrap();
    let state = directory.path().join("state");
    producer::typescript(&root, &state, &Default::default()).unwrap();
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let value = engine
        .execute(&Query {
            action: "refs".into(),
            kind: Some("call".into()),
            evidence: Some("combined".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["items"][0]["range"],
        serde_json::json!([1, 33, 1, 39])
    );
    assert_eq!(value["items"][0]["reason"], "semantic_static");
}
