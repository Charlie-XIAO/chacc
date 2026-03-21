//! AST node definitions.

/// A local variable stored in the current function's stack frame.
#[derive(Debug, Eq, PartialEq)]
pub struct LocalVar {
    pub name: String,
    /// The offset of the variable from the base pointer (RBP) in bytes.
    pub offset: i32,
}

/// The parsed program.
#[derive(Debug, Eq, PartialEq)]
pub struct Program {
    pub body: Vec<Stmt>,
    /// The local variable table used by the program.
    pub locals: Vec<LocalVar>,
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
}

/// The specific expression form carried by [`Node`].
#[derive(Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// A numeric literal.
    Num(i64),
    /// An address-of expression.
    Addr(Box<Node>),
    /// A pointer dereference.
    Deref(Box<Node>),
    /// A unary negation.
    Neg(Box<Node>),
    /// A local variable.
    ///
    /// The `usize` is the local variable's ID, which is an index into the
    /// program's local variable table [`Program::locals`].
    Var(usize),
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
            kind: NodeKind::Addr(Box::new(node)),
        }
    }

    /// Construct a dereference node.
    pub fn deref(node: Node, offset: usize) -> Self {
        Self {
            offset,
            kind: NodeKind::Deref(Box::new(node)),
        }
    }

    /// Construct a binary AST node.
    pub fn binary(op: BinaryOp, lhs: Node, rhs: Node, offset: usize) -> Self {
        Self {
            offset,
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
            kind: NodeKind::Neg(Box::new(node)),
        }
    }

    /// Construct an assignment node.
    pub fn assign(lhs: Node, rhs: Node, offset: usize) -> Self {
        Self {
            offset,
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
            kind: NodeKind::Num(value),
        }
    }

    /// Construct a local-variable node.
    pub fn var(local_id: usize, offset: usize) -> Self {
        Self {
            offset,
            kind: NodeKind::Var(local_id),
        }
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
