//! AST node definitions shared by the parser and code generator.

/// A local variable stored in the current function's stack frame.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LocalVar {
    pub(crate) name: String,
    pub(crate) offset: i32,
}

/// The parsed program plus its local-variable table.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Program {
    pub(crate) body: Vec<Stmt>,
    pub(crate) locals: Vec<LocalVar>,
}

/// Binary operators supported by the current expression grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
}

/// Expression nodes produced by the parser.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Node {
    Num(i64),
    Neg(Box<Node>),
    /// Index into the program's local-variable table.
    Var(usize),
    Assign {
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
}

impl Node {
    /// Construct a binary AST node.
    pub(crate) fn binary(op: BinaryOp, lhs: Node, rhs: Node) -> Self {
        Self::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// Construct a unary negation node.
    pub(crate) fn neg(node: Node) -> Self {
        Self::Neg(Box::new(node))
    }

    /// Construct an assignment node.
    pub(crate) fn assign(lhs: Node, rhs: Node) -> Self {
        Self::Assign {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }
}

/// Statements supported by the current language.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Stmt {
    Expr(Node),
    Return(Node),
}
