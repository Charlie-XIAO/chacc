//! AST node definitions.

use std::rc::Rc;

use smol_str::SmolStr;

use crate::types::{Member, Type};

/// The parsed program.
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<Function>,
    pub globals: Vec<GlobalVar>,
}

/// A function declaration or definition in [`Program`].
#[derive(Debug)]
pub struct Function {
    pub name: SmolStr,
    pub ty: Type,
    /// The function body.
    ///
    /// If this is `Some`, then this is a function definition. Otherwise, this
    /// is a function declaration.
    pub body: Option<Stmt>,
    /// Parameter local IDs in declaration order.
    pub param_locals: Vec<usize>,
    /// The local variable table used by the function.
    pub locals: Vec<LocalVar>,
}

/// A global variable defined in [`Program`].
#[derive(Debug)]
pub struct GlobalVar {
    pub name: SmolStr,
    pub ty: Type,
    /// Initial bytes for statically initialized data.
    pub init_data: Option<Rc<[u8]>>,
}

/// A local variable stored in a function's stack frame.
#[derive(Debug)]
pub struct LocalVar {
    pub _name: SmolStr,
    pub ty: Type,
    /// The offset of the variable from the base pointer (RBP) in bytes.
    pub offset: i64,
}

/// Reference to a named entity expression.
#[derive(Clone, Copy, Debug)]
pub enum EntityRef {
    /// A local variable, identified by its index in the function's local
    /// variable table [`Function::locals`].
    Local(usize),
    /// A global variable, identified by its index in the program's global
    /// variable table [`Program::globals`].
    Global(usize),
    /// A function, identified by its index in the program's function table
    /// [`Program::functions`].
    Function(usize),
}

/// Binary operators.
#[derive(Clone, Copy, Debug)]
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
#[derive(Debug, Default)]
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
#[derive(Debug, Default)]
pub enum NodeKind {
    /// A dummy node, used as temporary placeholders.
    #[default] // For ergonomics
    Dummy,
    /// A numeric literal.
    Num(i64),
    /// A function call.
    FuncCall { name: SmolStr, args: Vec<Node> },
    /// An address-of expression.
    Addr(Box<Node>),
    /// A pointer dereference.
    Deref(Box<Node>),
    /// A unary negation.
    Neg(Box<Node>),
    /// A reference to a named entity.
    ///
    /// Locals, globals, and functions are represented separately in
    /// [`Program`], but expression resolution can refer to any of them through
    /// this enum.
    Entity(EntityRef),
    /// An assignment.
    Assign { lhs: Box<Node>, rhs: Box<Node> },
    /// A comma operator for [generalized lvalues][1] as in GNU C Extension.
    ///
    /// [1]: https://gcc.gnu.org/onlinedocs/gcc-3.2.1/gcc/Lvalues.html
    Comma { lhs: Box<Node>, rhs: Box<Node> },
    /// A binary operation.
    Binary {
        op: BinaryOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
    /// A struct member.
    Member { parent: Box<Node>, member: Member },
    /// A [statement expression][1] as in GNU C Extension.
    ///
    /// [1]: https://gcc.gnu.org/onlinedocs/gcc/Statement-Exprs.html
    StmtExpr(Vec<Stmt>),
    /// A type cast.
    Cast(Box<Node>),
}

impl Node {
    /// Construct an address-of node.
    pub fn addr(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Addr(node.into()),
        }
    }

    /// Construct a dereference node.
    pub fn deref(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Deref(node.into()),
        }
    }

    /// Construct a binary AST node.
    pub fn binary(
        op: BinaryOp,
        lhs: impl Into<Box<Node>>,
        rhs: impl Into<Box<Node>>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Binary {
                op,
                lhs: lhs.into(),
                rhs: rhs.into(),
            },
        }
    }

    /// Construct a unary negation node.
    pub fn neg(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Neg(node.into()),
        }
    }

    /// Construct an assignment node.
    pub fn assign(lhs: impl Into<Box<Node>>, rhs: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Assign {
                lhs: lhs.into(),
                rhs: rhs.into(),
            },
        }
    }

    /// Construct a comma operator node.
    pub fn comma(lhs: impl Into<Box<Node>>, rhs: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Comma {
                lhs: lhs.into(),
                rhs: rhs.into(),
            },
        }
    }

    /// Construct a numeric literal node.
    ///
    /// This will automatically infer the node type. If `force_long` is true,
    /// the type of the node will always be `long`. Otherwise, it will be `int`
    /// if the value fits in an [`i32`] and otherwise `long`.
    pub fn num(value: i64, offset: usize, force_long: bool) -> Self {
        let ty = if !force_long && i32::try_from(value).is_ok() {
            Type::int()
        } else {
            Type::long()
        };

        Self {
            offset,
            ty: Some(ty),
            kind: NodeKind::Num(value),
        }
    }

    /// Construct a function call node.
    pub fn func_call(
        name: impl Into<SmolStr>,
        args: Vec<Node>,
        return_ty: Type,
        offset: usize,
    ) -> Self {
        debug_assert!(
            args.iter().all(|arg| arg.ty.is_some()),
            "not all children node types are set",
        );

        Self {
            offset,
            ty: Some(return_ty),
            kind: NodeKind::FuncCall {
                name: name.into(),
                args,
            },
        }
    }

    /// Construct an entity-reference node.
    pub fn entity(entity: EntityRef, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Entity(entity),
        }
    }

    /// Construct a struct member access node.
    pub fn member(parent: impl Into<Box<Node>>, member: Member, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Member {
                parent: parent.into(),
                member,
            },
        }
    }

    /// Construct a statement expression node.
    pub fn stmt_expr(stmts: Vec<Stmt>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::StmtExpr(stmts),
        }
    }

    /// Construct a type cast node.
    pub fn cast(expr: impl Into<Box<Node>>, ty: Type, offset: usize) -> Self {
        let expr = expr.into();
        debug_assert!(expr.ty.is_some(), "child node type is not set");

        Self {
            offset,
            ty: Some(ty),
            kind: NodeKind::Cast(expr),
        }
    }

    /// Get the type of this node, expecting it to be set.
    pub fn expect_ty(&self) -> &Type {
        self.ty.as_ref().expect("node type is not set")
    }
}

/// An AST node representing a statement.
#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    /// The offset from the start of the source code in bytes.
    pub offset: usize,
}

/// The specific statement form carried by [`Stmt`].
#[derive(Debug)]
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
