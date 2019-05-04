use super::{ident, type_name, CodeGenPass, TypeKind};
use crate::ast_pass::error::ErrorKind;
use graphql_parser::schema::*;
use heck::{CamelCase, SnakeCase};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

impl<'doc> CodeGenPass<'doc> {
    pub fn gen_query_trails(&mut self, doc: &'doc Document) {
        self.gen_query_trail();

        let fields_map = build_fields_map(doc);

        for def in &doc.definitions {
            if let Definition::TypeDefinition(type_def) = def {
                match type_def {
                    TypeDefinition::Object(obj) => {
                        self.gen_field_walk_methods(InternalQueryTrailNode::Object(obj))
                    }
                    TypeDefinition::Interface(interface) => {
                        self.gen_field_walk_methods(InternalQueryTrailNode::Interface(interface))
                    }
                    TypeDefinition::Union(union) => {
                        self.error_msg_if_field_types_dont_overlap(union, &fields_map);

                        self.gen_field_walk_methods(InternalQueryTrailNode::Union(
                            union,
                            build_union_fields_set(union, &fields_map),
                        ))
                    }
                    _ => {}
                }
            }
        }
    }

    fn gen_query_trail(&mut self) {
        self.extend(quote! {
            /// A wrapper around a `juniper::LookAheadSelection` with methods for each possible child.
            ///
            /// Generated by `juniper-from-schema`.
            pub struct QueryTrail<'a, T, K> {
                look_ahead: Option<&'a juniper::LookAheadSelection<'a, juniper::DefaultScalarValue>>,
                node_type: std::marker::PhantomData<T>,
                walked: K,
            }

            impl<'a, T> QueryTrail<'a, T, NotWalked> {
                /// Check if the trail is present in the query being executed
                ///
                /// Generated by `juniper-from-schema`.
                pub fn walk(self) -> Option<QueryTrail<'a, T, Walked>> {
                    match self.look_ahead {
                        Some(inner) => {
                            Some(QueryTrail {
                                look_ahead: Some(inner),
                                node_type: self.node_type,
                                walked: Walked,
                            })
                        },
                        None => None,
                    }
                }
            }

            /// A type used to parameterize `QueryTrail` to know that `walk` has been called.
            pub struct Walked;

            /// A type used to parameterize `QueryTrail` to know that `walk` has *not* been called.
            pub struct NotWalked;

            trait MakeQueryTrail<'a> {
                fn make_query_trail<T>(&'a self) -> QueryTrail<'a, T, Walked>;
            }

            impl<'a> MakeQueryTrail<'a> for juniper::LookAheadSelection<'a, juniper::DefaultScalarValue> {
                fn make_query_trail<T>(&'a self) -> QueryTrail<'a, T, Walked> {
                    QueryTrail {
                        look_ahead: Some(self),
                        node_type: std::marker::PhantomData,
                        walked: Walked,
                    }
                }
            }
        })
    }

    fn gen_field_walk_methods(&mut self, obj: InternalQueryTrailNode<'_>) {
        let name = ident(&obj.name());
        let fields = obj.fields();
        let methods = fields
            .iter()
            .map(|field| self.gen_field_walk_method(field))
            .collect::<Vec<_>>();

        self.extend(quote! {
            impl<'a, K> QueryTrail<'a, #name, K> {
                #(#methods)*
            }
        })
    }

    fn error_msg_if_field_types_dont_overlap(
        &mut self,
        union: &'doc UnionType,
        fields_map: &HashMap<&'doc String, Vec<&'doc Field>>,
    ) {
        let mut prev: HashMap<&'doc str, (&'doc str, &'doc str)> = HashMap::new();

        for type_b in &union.types {
            if let Some(fields) = fields_map.get(type_b) {
                for field in fields {
                    let field_type_b = type_name(&field.field_type);

                    if let Some((type_a, field_type_a)) = prev.get(&field.name.as_ref()) {
                        if field_type_b != field_type_a {
                            self.emit_non_fatal_error(
                                union.position,
                                ErrorKind::UnionFieldTypeMismatch {
                                    union_name: &union.name,
                                    field_name: &field.name,
                                    type_a: &type_a,
                                    type_b: &type_b,
                                    field_type_a: &field_type_a,
                                    field_type_b: &field_type_b,
                                },
                            );
                        }
                    }

                    prev.insert(&field.name, (type_b, field_type_b));
                }
            }
        }
    }

    fn gen_field_walk_method(&mut self, field: &Field) -> TokenStream {
        let field_type = type_name(&field.field_type);
        let (_, ty) = self.graphql_scalar_type_to_rust_type(&field_type, field.position);
        let field_type = ident(field_type.clone().to_camel_case());

        match ty {
            TypeKind::Scalar => {
                let name = ident(&field.name.to_snake_case());
                let string_name = &field.name;

                quote! {
                    /// Check if a scalar leaf node is queried for
                    ///
                    /// Generated by `juniper-from-schema`.
                    pub fn #name(&self) -> bool {
                        use juniper::LookAheadMethods;

                        self.look_ahead
                            .and_then(|la| la.select_child(#string_name))
                            .is_some()
                    }
                }
            }
            TypeKind::Type => {
                let name = ident(&field.name.to_snake_case());
                let string_name = &field.name;

                quote! {
                    /// Walk the trail into a field.
                    ///
                    /// Generated by `juniper-from-schema`.
                    pub fn #name(&self) -> QueryTrail<'a, #field_type, NotWalked> {
                        use juniper::LookAheadMethods;

                        let child = self.look_ahead.and_then(|la| la.select_child(#string_name));

                        QueryTrail {
                            look_ahead: child,
                            node_type: std::marker::PhantomData,
                            walked: NotWalked,
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct HashFieldByName<'a>(&'a Field);

impl<'a> PartialEq for HashFieldByName<'a> {
    fn eq(&self, other: &HashFieldByName) -> bool {
        self.0.name == other.0.name
    }
}

impl<'a> Eq for HashFieldByName<'a> {}

impl<'a> Hash for HashFieldByName<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.name.hash(state);
    }
}

enum InternalQueryTrailNode<'a> {
    Object(&'a ObjectType),
    Interface(&'a InterfaceType),
    Union(&'a UnionType, HashSet<HashFieldByName<'a>>),
}

impl<'a> InternalQueryTrailNode<'a> {
    fn name(&self) -> &String {
        match self {
            InternalQueryTrailNode::Object(inner) => &inner.name,
            InternalQueryTrailNode::Interface(inner) => &inner.name,
            InternalQueryTrailNode::Union(inner, _fields) => &inner.name,
        }
    }

    fn fields(&self) -> Vec<&'a Field> {
        match self {
            InternalQueryTrailNode::Object(inner) => inner.fields.iter().collect(),
            InternalQueryTrailNode::Interface(inner) => inner.fields.iter().collect(),
            InternalQueryTrailNode::Union(_inner, fields) => fields
                .iter()
                .map(|hashable_field| hashable_field.0)
                .collect(),
        }
    }
}

fn build_union_fields_set<'d>(
    union: &UnionType,
    fields_map: &HashMap<&'d String, Vec<&'d Field>>,
) -> HashSet<HashFieldByName<'d>> {
    let mut union_fields_set = HashSet::new();

    for type_ in &union.types {
        if let Some(fields) = fields_map.get(type_) {
            for field in fields {
                union_fields_set.insert(HashFieldByName(&field));
            }
        }
    }

    union_fields_set
}

fn build_fields_map(doc: &Document) -> HashMap<&String, Vec<&Field>> {
    let mut map = HashMap::new();

    for def in &doc.definitions {
        if let Definition::TypeDefinition(type_def) = def {
            if let TypeDefinition::Object(obj) = type_def {
                for field in &obj.fields {
                    let entry = map.entry(&obj.name).or_insert_with(|| vec![]);
                    entry.push(field);
                }
            }
        }
    }

    map
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ast_pass::AstData;

    #[test]
    fn test_fails_to_generate_query_trail_for_unions_where_fields_dont_overlap() {
        let schema = r#"
            union Entity = User | Company

            type User {
              country: Country!
            }

            type Company {
              country: OtherCountry!
            }

            type Country {
              id: Int!
            }

            type OtherCountry {
              id: Int!
            }
        "#;

        let doc = graphql_parser::parse_schema(&schema).unwrap();
        let ast_data = AstData::new(&doc);
        let mut out = CodeGenPass {
            tokens: quote! {},
            error_type: crate::parse_input::default_error_type(),
            ast_data,
            errors: std::collections::BTreeSet::new(),
            raw_schema: schema,
        };

        out.gen_query_trails(&doc);

        assert_eq!(1, out.errors.len());
    }
}
