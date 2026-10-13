//! Supported wire fields from https://github.com/scip-code/scip/blob/main/scip.proto
//! (SCIP's Apache-2.0 schema, inspected 2026-10-13). Unknown protobuf fields are
//! ignored. No protoc or build-time schema download is needed.

#[derive(Clone, PartialEq, prost::Message)]
pub struct Index {
    #[prost(message, optional, tag = "1")]
    pub metadata: Option<Metadata>,
    #[prost(message, repeated, tag = "2")]
    pub documents: Vec<Document>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Metadata {
    #[prost(int32, tag = "1")]
    pub version: i32,
    #[prost(message, optional, tag = "2")]
    pub tool_info: Option<ToolInfo>,
    #[prost(string, tag = "3")]
    pub project_root: String,
    #[prost(int32, tag = "4")]
    pub text_document_encoding: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct ToolInfo {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
    // Arguments are deliberately not decoded or persisted: they can contain secrets.
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Document {
    #[prost(string, tag = "1")]
    pub relative_path: String,
    #[prost(message, repeated, tag = "2")]
    pub occurrences: Vec<Occurrence>,
    #[prost(message, repeated, tag = "3")]
    pub symbols: Vec<SymbolInformation>,
    #[prost(string, tag = "4")]
    pub language: String,
    #[prost(string, tag = "5")]
    pub text: String,
    #[prost(int32, tag = "6")]
    pub position_encoding: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Occurrence {
    #[prost(int32, repeated, tag = "1")]
    pub range: Vec<i32>,
    #[prost(string, tag = "2")]
    pub symbol: String,
    #[prost(int32, tag = "3")]
    pub symbol_roles: i32,
    #[prost(oneof = "occurrence::TypedRange", tags = "8, 9")]
    pub typed_range: Option<occurrence::TypedRange>,
}
pub mod occurrence {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum TypedRange {
        #[prost(message, tag = "8")]
        Single(super::SingleLineRange),
        #[prost(message, tag = "9")]
        Multi(super::MultiLineRange),
    }
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct SingleLineRange {
    #[prost(int32, tag = "1")]
    pub line: i32,
    #[prost(int32, tag = "2")]
    pub start_character: i32,
    #[prost(int32, tag = "3")]
    pub end_character: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct MultiLineRange {
    #[prost(int32, tag = "1")]
    pub start_line: i32,
    #[prost(int32, tag = "2")]
    pub start_character: i32,
    #[prost(int32, tag = "3")]
    pub end_line: i32,
    #[prost(int32, tag = "4")]
    pub end_character: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct SymbolInformation {
    #[prost(string, tag = "1")]
    pub symbol: String,
    #[prost(message, repeated, tag = "4")]
    pub relationships: Vec<Relationship>,
    #[prost(int32, tag = "5")]
    pub kind: i32,
    #[prost(string, tag = "6")]
    pub display_name: String,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Relationship {
    #[prost(string, tag = "1")]
    pub symbol: String,
    #[prost(bool, tag = "3")]
    pub is_implementation: bool,
}
