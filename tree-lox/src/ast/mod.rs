macro_rules! make_ast_enum {
    ( $enum_name:ident, [ $( $variant:ident ),* $( , )? ] ) => {
        #[derive(Debug, Clone)]
        pub enum $enum_name {
            $( $variant($variant), )*
        }
        impl $enum_name {
            /// Returns the span of the inner AST node.
            #[inline]
            pub fn span(&self) -> Span {
                match self {
                    $(
                        $enum_name::$variant(inner) => inner.span,
                    )*
                }
            }
        }
        $(
            impl From<$variant> for $enum_name {
                fn from(val: $variant) -> $enum_name {
                    $enum_name::$variant(val)
                }
            }
            #[allow(clippy::from_over_into)]
            impl Into<Box<$enum_name>> for $variant {
                fn into(self) -> Box<$enum_name> {
                    Box::new($enum_name::from(self))
                }
            }
        )*
    }
}

pub mod dbg;
pub mod expr;
pub mod stmt;