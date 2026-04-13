//! AST node definitions.

use std::rc::Rc;

use smol_str::SmolStr;

use crate::types::{Member, Type, TypeStore};

/// The parsed program.
#[derive(Debug)]
pub struct Program {
    pub types: TypeStore,
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
    /// The hidden `__va_area__` local for variadic functions.
    pub va_area_local: Option<usize>,
    /// The local variable table used by the function.
    pub locals: Vec<LocalVar>,
    pub is_static: bool,
}

/// A relocation inside statically initialized global data.
#[derive(Debug)]
pub struct Relocation {
    pub offset: usize,
    pub label: SmolStr,
    pub addend: i64,
}

/// Static initialization data of a global variable.
#[derive(Debug)]
pub struct GlobalInitData {
    pub bytes: Rc<[u8]>,
    pub relocations: Rc<[Relocation]>,
}

/// Storage state of a global variable.
#[derive(Debug)]
pub enum GlobalStorage {
    /// Declaration only, no storage.
    Decl,
    /// Zero-initialized storage.
    Zero,
    /// Explicit static intialization data.
    Data(GlobalInitData),
}

impl GlobalStorage {
    fn rank(&self) -> u8 {
        match self {
            Self::Decl => 0,
            Self::Zero => 1,
            Self::Data(_) => 2,
        }
    }

    /// Merge another state into this one.
    ///
    /// This returns true if there is a redefinition error.
    pub fn merge(&mut self, other: Self) -> bool {
        if matches!(self, Self::Data(_)) && matches!(other, Self::Data(_)) {
            return true;
        }
        if other.rank() > self.rank() {
            *self = other;
        }
        false
    }
}

/// A global variable defined in [`Program`].
#[derive(Debug)]
pub struct GlobalVar {
    pub name: SmolStr,
    pub ty: Type,
    /// Optional alignment override via "_Alignas".
    pub align: Option<i64>,
    pub storage: GlobalStorage,
    pub is_static: bool,
}

/// A local variable stored in a function's stack frame.
#[derive(Debug)]
pub struct LocalVar {
    pub _name: SmolStr,
    pub ty: Type,
    /// Optional alignment override via "_Alignas".
    pub align: Option<i64>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    BitLeftShift,
    BitRightShift,
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
    /// An address-of expression "&".
    Addr(Box<Node>),
    /// A pointer dereference "*".
    Deref(Box<Node>),
    /// A unary negation operation "-".
    Neg(Box<Node>),
    /// A unary not operation "!".
    Not(Box<Node>),
    /// A unary bit-not operation "~".
    BitNot(Box<Node>),
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
    /// A logical and operation "&&".
    LogicalAnd { lhs: Box<Node>, rhs: Box<Node> },
    /// A logical or operation "||".
    LogicalOr { lhs: Box<Node>, rhs: Box<Node> },
    /// A binary operation.
    Binary {
        op: BinaryOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
    /// A conditional operator "?".
    Conditional {
        cond: Box<Node>,
        then_expr: Box<Node>,
        else_expr: Box<Node>,
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
    /// Construct a numeric literal node.
    ///
    /// This will automatically infer the node type. If `force_long` is true,
    /// the type of the node will always be `long`. Otherwise, it will be `int`
    /// if the value fits in an [`i32`] and otherwise `long`.
    pub fn num(value: i64, offset: usize, force_long: bool) -> Self {
        let ty = if !force_long && i32::try_from(value).is_ok() {
            Type::Int
        } else {
            Type::Long
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

    /// Construct a unary negation node.
    pub fn neg(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Neg(node.into()),
        }
    }

    /// Construct a unary not node.
    pub fn not(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Not(node.into()),
        }
    }

    /// Construct a unary bit-not node.
    pub fn bit_not(node: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::BitNot(node.into()),
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

    /// Construct a logical and node.
    pub fn logical_and(
        lhs: impl Into<Box<Node>>,
        rhs: impl Into<Box<Node>>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::LogicalAnd {
                lhs: lhs.into(),
                rhs: rhs.into(),
            },
        }
    }

    /// Construct a logical or node.
    pub fn logical_or(lhs: impl Into<Box<Node>>, rhs: impl Into<Box<Node>>, offset: usize) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::LogicalOr {
                lhs: lhs.into(),
                rhs: rhs.into(),
            },
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

    /// Construct a conditional node.
    pub fn conditional(
        cond: impl Into<Box<Node>>,
        then_expr: impl Into<Box<Node>>,
        else_expr: impl Into<Box<Node>>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            ty: None,
            kind: NodeKind::Conditional {
                cond: cond.into(),
                then_expr: then_expr.into(),
                else_expr: else_expr.into(),
            },
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
    pub fn expect_ty(&self) -> Type {
        self.ty.expect("node type is not set")
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
    Return(Option<Node>),
    /// A loop statement.
    Loop {
        /// Initialization statement, only used optionally for for-loops.
        init: Option<Box<Stmt>>,
        /// Loop condition, optional for for-loops.
        cond: Option<Node>,
        /// Loop increment, only used optionally for for-loops.
        inc: Option<Node>,
        /// Loop body.
        body: Box<Stmt>,
        /// Whether this is a do-while loop.
        do_while: bool,
        brk_label: SmolStr,
        cont_label: SmolStr,
    },
    /// An if-else statement.
    If {
        cond: Node,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    /// A switch statement as in switch-case.
    Switch {
        cond: Node,
        body: Box<Stmt>,
        /// The cases within this switch.
        ///
        /// Each element corresponds to the case value and the corresponding
        /// [`StmtKind::Label::label`].
        cases: Vec<(i64, SmolStr)>,
        /// The default case label within this switch.
        default: Option<SmolStr>,
        brk_label: SmolStr,
    },
    /// A block statement.
    Block(Vec<Stmt>),
    /// A jump statement, like goto, break, and continue.
    Jump {
        /// The target label in assembly.
        ///
        /// This must be set before codegen, and is `Option` because goto
        /// statements cannot be resolved upon creation.
        label: Option<SmolStr>,
        /// The label name to jump to, used by goto.
        label_name: Option<SmolStr>,
    },
    /// A label statement, including case and default as in switch-case.
    Label {
        /// The label in assembly.
        label: SmolStr,
        body: Box<Stmt>,
        /// The label name, used by actual label statement.
        name: Option<SmolStr>,
    },
    /// Zero-fill a local variable, given its ID.
    MemzeroLocal(usize),
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
    pub fn return_(expr: Option<Node>, offset: usize) -> Self {
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
        brk_label: SmolStr,
        cont_label: SmolStr,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::Loop {
                init: Some(init),
                cond,
                inc,
                body,
                do_while: false,
                brk_label,
                cont_label,
            },
        }
    }

    /// Construct a while-loop or do-while statement.
    pub fn while_(
        cond: Node,
        body: Box<Stmt>,
        do_while: bool,
        brk_label: SmolStr,
        cont_label: SmolStr,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::Loop {
                init: None,
                cond: Some(cond),
                inc: None,
                body,
                do_while,
                brk_label,
                cont_label,
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

    /// Construct a switch statement.
    pub fn switch(
        cond: Node,
        body: Box<Stmt>,
        cases: Vec<(i64, SmolStr)>,
        default: Option<SmolStr>,
        brk_label: SmolStr,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::Switch {
                cond,
                body,
                cases,
                default,
                brk_label,
            },
        }
    }

    /// Construct a goto statement.
    pub fn goto(label_name: impl Into<SmolStr>, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Jump {
                label: None,
                label_name: Some(label_name.into()),
            },
        }
    }

    /// Construct a break/continue statement.
    pub fn jump(label: impl Into<SmolStr>, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Jump {
                label: Some(label.into()),
                label_name: None,
            },
        }
    }

    /// Construct a label statement.
    pub fn label(
        label: impl Into<SmolStr>,
        body: Box<Stmt>,
        name: impl Into<SmolStr>,
        offset: usize,
    ) -> Self {
        Self {
            offset,
            kind: StmtKind::Label {
                label: label.into(),
                body,
                name: Some(name.into()),
            },
        }
    }

    /// Construct a case statement.
    pub fn case(label: impl Into<SmolStr>, body: Box<Stmt>, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::Label {
                label: label.into(),
                body,
                name: None,
            },
        }
    }

    /// Construct a statement that zero-fills a local variable.
    pub fn memzero_local(local_id: usize, offset: usize) -> Self {
        Self {
            offset,
            kind: StmtKind::MemzeroLocal(local_id),
        }
    }
}
