//! AST node definitions.

use crate::types::Type;

/// The parsed program.
#[derive(Debug, Eq, PartialEq)]
pub struct Program {
    pub functions: Vec<Function>,
    pub globals: Vec<GlobalVar>,
}

/// A function defined in [`Program`].
#[derive(Debug, Eq, PartialEq)]
pub struct Function {
    pub name: String,
    /// Parameter local IDs in declaration order.
    pub params: Vec<usize>,
    pub body: Stmt,
    /// The local variable table used by the function.
    pub locals: Vec<LocalVar>,
}

/// A global variable defined in [`Program`].
#[derive(Debug, Eq, PartialEq)]
pub struct GlobalVar {
    pub name: String,
    pub ty: Type,
}

/// A local variable stored in a function's stack frame.
#[derive(Debug, Eq, PartialEq)]
pub struct LocalVar {
    pub name: String,
    pub ty: Type,
    /// The offset of the variable from the base pointer (RBP) in bytes.
    pub offset: i64,
}

/// Reference to a variable expression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VarRef {
    /// A local variable, identified by its index in the function's local
    /// variable table [`Function::locals`].
    Local(usize),
    /// A global variable, identified by its index in the program's global
    /// variable table [`Program::globals`].
    Global(usize),
}

/// Binary operators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
}

/// An AST node representing an expression.
#[derive(Debug, Eq, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    /// The offset from the start of the source code in bytes.
    pub offset: usize,
    /// The type computed for this expression.
    ///
    /// This is `Option` because it is not set during parsing, but only during
    /// type checking.
    pub ty: Option<Type>,
}

/// The specific expression form carried by [`Node`].
#[derive(Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// A numeric literal.
    Num(i64),
    /// A function call.
    FuncCall { name: String, args: Vec<Node> },
    /// An address-of expression.
    Addr(Box<Node>),
    /// A pointer dereference.
    Deref(Box<Node>),
    /// A unary negation.
    Neg(Box<Node>),
    /// A reference to a local or global variable.
    ///
    /// They are represented separately in [`Program`], but expression
    /// resolution can refer to either through this enum.
    Var(VarRef),
    /// An assignment.
    Assign { lhs: Box<Node>, rhs: Box<Node> },
    /// A binary operation.
    Binary {
        op: BinaryOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
}

impl Node {
    /// Construct an address-of node.
    pub fn addr(node: Node, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Addr(Box::new(node)),
        }
    }

    /// Construct a dereference node.
    pub fn deref(node: Node, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Deref(Box::new(node)),
        }
    }

    /// Construct a binary AST node.
    pub fn binary(op: BinaryOp, lhs: Node, rhs: Node, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    /// Construct a unary negation node.
    pub fn neg(node: Node, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Neg(Box::new(node)),
        }
    }

    /// Construct an assignment node.
    pub fn assign(lhs: Node, rhs: Node, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Assign {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    /// Construct a numeric literal node.
    pub fn num(value: i64, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Num(value),
        }
    }

    /// Construct a function call node.
    pub fn func_call(name: String, args: Vec<Node>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::FuncCall { name, args },
        }
    }

    /// Construct a variable-reference node.
    pub fn var(var: VarRef, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Var(var),
        }
    }

    /// Get the type of this node, expecting it to be set.
    pub fn expect_ty(&self) -> &Type {
        self.ty.as_ref().expect("node data type is not set")
    }
}

/// An AST node representing a statement.
#[derive(Debug, Eq, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    /// The offset from the start of the source code in bytes.
    pub offset: usize,
}

/// The specific statement form carried by [`Stmt`].
#[derive(Debug, Eq, PartialEq)]
pub enum StmtKind {
    /// An expression statement.
    Expr(Node),
    /// A return statement.
    Return(Node),
    /// A for-loop or while-loop statement.
    Loop {
        /// Initialization statement, only used optionally for for-loops.
        init: Option<Box<Stmt>>,
        /// Loop condition, optional for for-loops.
        cond: Option<Node>,
        /// Loop increment, only used optionally for for-loops.
        inc: Option<Node>,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// An if-else statement.
    If {
        cond: Node,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    /// A block statement.
    Block(Vec<Stmt>),
}

impl Stmt {
    /// Construct an expression statement.
    pub fn expr(expr: Node, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Expr(expr),
        }
    }

    /// Construct a return statement.
    pub fn return_(expr: Node, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Return(expr),
        }
    }

    /// Construct a block statement.
    pub fn block(stmts: Vec<Stmt>, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Block(stmts),
        }
    }

    /// Construct a for-loop statement.
    pub fn for_(
        init: Box<Stmt>,
        cond: Option<Node>,
        inc: Option<Node>,
        body: Box<Stmt>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::Loop {
                init: Some(init),
                cond,
                inc,
                body,
            },
        }
    }

    /// Construct a while-loop statement.
    pub fn while_(cond: Node, body: Box<Stmt>, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Loop {
                init: None,
                cond: Some(cond),
                inc: None,
                body,
            },
        }
    }

    /// Construct a conditional statement.
    pub fn if_(
        cond: Node,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch,
            },
        }
    }
}
