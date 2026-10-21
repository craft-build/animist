pub mod catalog;
pub mod provider;

#[macro_use]
extern crate macro_rules_attribute;

derive_alias! {
 #[derive(Struct!)] = #[derive(Debug, Clone, Default, PartialEq)];
 #[derive(Enum!)] = #[derive(Debug, Clone, PartialEq, Eq)];
}
