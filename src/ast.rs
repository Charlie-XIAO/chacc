//! AST node definitions shared by the parser and code generator.

/// A local variable stored in the current function's stack frame.
#[derive(Debug, Eq, PartialEq)]
pub struct LocalVar {
    pub name: String,
    pub offset: i32,
}

/// The parsed program plus its local-variable table.
#[derive(Debug, Eq, PartialEq)]
pub struct Program {
    pub body: Vec<Stmt>,
    pub locals: Vec<LocalVar>,
}

/// Binary operators supported by the current expression grammar.
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

/// Expression nodes produced by the parser.
#[derive(Debug, Eq, PartialEq)]
pub struct Node {
    pub offset: usize,
    pub kind: NodeKind,
}

/// The specific expression form carried by a node.
#[derive(Debug, Eq, PartialEq)]
pub enum NodeKind {
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

/// Statements supported by the current language.
#[derive(Debug, Eq, PartialEq)]
pub struct Stmt {
    pub offset: usize,
    pub kind: StmtKind,
}

/// The specific statement form carried by a statement node.
#[derive(Debug, Eq, PartialEq)]
pub enum StmtKind {
    Expr(Node),
    Return(Node),
    Loop {
        init: Option<Box<Stmt>>,
        cond: Option<Node>,
        inc: Option<Node>,
        body: Box<Stmt>,
    },
    If {
        cond: Node,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
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
