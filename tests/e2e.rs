use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::tempdir;

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

trait CommandExt {
    fn cc() -> Command {
        let cc = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
        Command::new(cc)
    }

    fn chacc() -> Command {
        Command::new(env!("CARGO_BIN_EXE_chacc"))
    }

    fn run_checked(&mut self, what: &str) -> Output;
}

impl CommandExt for Command {
    fn run_checked(&mut self, what: &str) -> Output {
        let output = self
            .output()
            .unwrap_or_else(|err| panic!("{what} failed to start: {err}"));

        let context = format!(
            "command: {self:?}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        assert!(output.status.success(), "{what} failed\n{context}",);
        output
    }
}

#[derive(Debug, Default)]
struct Fixture {
    source: String,
}

impl Fixture {
    fn new() -> Self {
        let mut f = Self::default();
        f.line("#include \"test.h\"");
        f
    }

    fn line(&mut self, content: &str) {
        self.source.push_str(content);
        if !content.ends_with('\n') {
            self.source.push('\n');
        }
    }

    fn main(&mut self) {
        self.line("int main() {");
    }

    fn assert<E, A>(&mut self, expected: E, actual: A)
    where
        E: std::fmt::Display,
        A: std::fmt::Display,
    {
        self.line(&format!("  ASSERT({expected}, {actual});"));
    }

    fn finish(&mut self) {
        self.line("  return 0;");
        self.line("}");
    }

    fn run(&self, stem: &str) {
        let tests_dir = tests_dir();
        let tmp = tempdir().expect("failed to create temporary directory");
        let source = tmp.path().join(format!("{stem}.c"));
        let preprocessed = tmp.path().join(format!("{stem}.i"));
        let asm = tmp.path().join(format!("{stem}.s"));
        let exe = tmp.path().join(stem);

        std::fs::write(&source, &self.source).expect("failed to write fixture");

        Command::cc()
            .args([
                OsStr::new("-E"),
                OsStr::new("-P"),
                OsStr::new("-C"),
                OsStr::new("-I"),
                tests_dir.as_os_str(),
                source.as_os_str(),
                OsStr::new("-o"),
                preprocessed.as_os_str(),
            ])
            .run_checked(&format!("preprocessing {}", source.display()));

        Command::chacc()
            .arg("-o")
            .arg(&asm)
            .arg(&preprocessed)
            .run_checked(&format!("compiling {}", source.display()));

        Command::cc()
            .arg("-o")
            .arg(&exe)
            .arg(&asm)
            .arg(tests_dir.join("test.c"))
            .run_checked(&format!("linking {}", source.display()));

        Command::new(&exe).run_checked(&format!(
            "running {}",
            source.file_name().unwrap().to_string_lossy()
        ));
    }
}

#[rustfmt::skip]
#[test]
fn test_arith() {
    let mut f = Fixture::new();
    f.main();

    f.assert(0, "0");
    f.assert(42, "42");
    f.assert(21, "5+20-4");
    f.assert(41, " 12 + 34 - 5 ");
    f.assert(47, "5+6*7");
    f.assert(15, "5*(9-6)");
    f.assert(4, "(3+5)/2");
    f.assert(10, "-10+20");
    f.assert(10, "- -10");
    f.assert(10, "- - +10");

    f.assert(0, "0==1");
    f.assert(1, "42==42");
    f.assert(1, "0!=1");
    f.assert(0, "42!=42");

    f.assert(1, "0<1");
    f.assert(0, "1<1");
    f.assert(0, "2<1");
    f.assert(1, "0<=1");
    f.assert(1, "1<=1");
    f.assert(0, "2<=1");

    f.assert(1, "1>0");
    f.assert(0, "1>1");
    f.assert(0, "1>2");
    f.assert(1, "1>=0");
    f.assert(1, "1>=1");
    f.assert(0, "1>=2");

    f.assert(0, "1073741824 * 100 / 100");

    f.assert(7, "({ int i=2; i+=5; i; })");
    f.assert(7, "({ int i=2; i+=5; })");
    f.assert(3, "({ int i=5; i-=2; i; })");
    f.assert(3, "({ int i=5; i-=2; })");
    f.assert(6, "({ int i=3; i*=2; i; })");
    f.assert(6, "({ int i=3; i*=2; })");
    f.assert(3, "({ int i=6; i/=2; i; })");
    f.assert(3, "({ int i=6; i/=2; })");

    f.assert(3, "({ int i=2; ++i; })");
    f.assert(2, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; ++*p; })");
    f.assert(0, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; --*p; })");

    f.assert(2, "({ int i=2; i++; })");
    f.assert(2, "({ int i=2; i--; })");
    f.assert(3, "({ int i=2; i++; i; })");
    f.assert(1, "({ int i=2; i--; i; })");
    f.assert(1, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; *p++; })");
    f.assert(1, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; *p--; })");

    f.assert(0, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p++)--; a[0]; })");
    f.assert(0, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*(p--))--; a[1]; })");
    f.assert(2, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p)--; a[2]; })");
    f.assert(2, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p)--; p++; *p; })");

    f.assert(0, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p++)--; a[0]; })");
    f.assert(0, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p++)--; a[1]; })");
    f.assert(2, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p++)--; a[2]; })");
    f.assert(2, "({ int a[3]; a[0]=0; a[1]=1; a[2]=2; int *p=a+1; (*p++)--; *p; })");

    f.finish();
    f.run("arith");
}

#[rustfmt::skip]
#[test]
fn test_cast() {
    let mut f = Fixture::new();
    f.main();

    f.assert(131585, "(int)8590066177");
    f.assert(513, "(short)8590066177");
    f.assert(1, "(char)8590066177");
    f.assert(1, "(long)1");
    f.assert(0, "(long)&*(int *)0");
    f.assert(513, "({ int x=512; *(char *)&x=1; x; })");
    f.assert(5, "({ int x=5; long y=(long)&x; *(int*)y; })");

    f.line("(void)1;");

    f.finish();
    f.run("cast");
}

#[rustfmt::skip]
#[test]
fn test_control() {
    let mut f = Fixture::new();
    f.line("/*");
    f.line(" * This is a block comment.");
    f.line(" */");
    f.main();

    f.assert(3, "({ int x; if (0) x=2; else x=3; x; })");
    f.assert(3, "({ int x; if (1-1) x=2; else x=3; x; })");
    f.assert(2, "({ int x; if (1) x=2; else x=3; x; })");
    f.assert(2, "({ int x; if (2-1) x=2; else x=3; x; })");
    f.assert(55, "({ int i=0; int j=0; for (i=0; i<=10; i=i+1) j=i+j; j; })");
    f.assert(10, "({ int i=0; while(i<10) i=i+1; i; })");
    f.assert(3, "({ 1; {2;} 3; })");
    f.assert(5, "({ ;;; 5; })");
    f.assert(10, "({ int i=0; while(i<10) i=i+1; i; })");
    f.assert(55, "({ int i=0; int j=0; while(i<=10) {j=i+j; i=i+1;} j; })");

    f.assert(3, "(1,2,3)");
    f.assert(5, "({ int i=2, j=3; (i=5,j)=6; i; })");
    f.assert(6, "({ int i=2, j=3; (i=5,j)=6; j; })");

    f.assert(55, "({ int j=0; for (int i=0; i<=10; i=i+1) j=j+i; j; })");
    f.assert(3, "({ int i=3; int j=0; for (int i=0; i<=10; i=i+1) j=j+i; i; })");

    f.finish();
    f.run("control");
}

#[rustfmt::skip]
#[test]
fn test_decl() {
    let mut f = Fixture::new();
    f.main();

    f.assert(1, "({ char x; sizeof(x); })");
    f.assert(2, "({ short int x; sizeof(x); })");
    f.assert(2, "({ int short x; sizeof(x); })");
    f.assert(4, "({ int x; sizeof(x); })");
    f.assert(8, "({ long int x; sizeof(x); })");
    f.assert(8, "({ int long x; sizeof(x); })");

    f.assert(8, "({ long long x; sizeof(x); })");

    f.assert(0, "({ _Bool x=0; x; })");
    f.assert(1, "({ _Bool x=1; x; })");
    f.assert(1, "({ _Bool x=2; x; })");
    f.assert(1, "(_Bool)1");
    f.assert(1, "(_Bool)2");
    f.assert(0, "(_Bool)(char)256");

    f.finish();
    f.run("decl");
}

#[rustfmt::skip]
#[test]
fn test_enum() {
    let mut f = Fixture::new();
    f.main();

    f.assert(0, "({ enum { zero, one, two }; zero; })");
    f.assert(1, "({ enum { zero, one, two }; one; })");
    f.assert(2, "({ enum { zero, one, two }; two; })");
    f.assert(5, "({ enum { five=5, six, seven }; five; })");
    f.assert(6, "({ enum { five=5, six, seven }; six; })");
    f.assert(0, "({ enum { zero, five=5, three=3, four }; zero; })");
    f.assert(5, "({ enum { zero, five=5, three=3, four }; five; })");
    f.assert(3, "({ enum { zero, five=5, three=3, four }; three; })");
    f.assert(4, "({ enum { zero, five=5, three=3, four }; four; })");
    f.assert(4, "({ enum { zero, one, two } x; sizeof(x); })");
    f.assert(4, "({ enum t { zero, one, two }; enum t y; sizeof(y); })");

    f.finish();
    f.run("enum");
}

#[rustfmt::skip]
#[test]
fn test_function() {
    let mut f = Fixture::new();
    f.line("int ret3() { return 3; return 5; }");
    f.line("int add2(int x, int y) { return x + y; }");
    f.line("int sub2(int x, int y) { return x - y; }");
    f.line("int add6(int a, int b, int c, int d, int e, int f) { return a + b + c + d + e + f; }");
    f.line("int addx(int *x, int y) { return *x + y; }");
    f.line("int sub_char(char a, char b, char c) { return a - b - c; }");
    f.line("int fib(int x) { if (x<=1) return 1; return fib(x-1) + fib(x-2); }");
    f.line("int sub_long(long a, long b, long c) { return a - b - c; }");
    f.line("int sub_short(short a, short b, short c) { return a - b - c; }");
    f.line("int g1;");
    f.line("int *g1_ptr() { return &g1; }");
    f.line("char int_to_char(int x) { return x; }");
    f.line("long first_long(long a, char b) { return a; }");
    f.line("int div_long(long a, long b) { return a / b; }");
    f.line("_Bool bool_fn_add(_Bool x) { return x + 1; }");
    f.line("_Bool bool_fn_sub(_Bool x) { return x - 1; }");
    f.line("static int static_fn() { return 3; }");
    f.main();

    f.assert(3, "ret3()");
    f.assert(8, "add2(3, 5)");
    f.assert(2, "sub2(5, 3)");
    f.assert(21, "add6(1,2,3,4,5,6)");
    f.assert(66, "add6(1,2,add6(3,4,5,6,7,8),9,10,11)");
    f.assert(136, "add6(1,2,add6(3,add6(4,5,6,7,8,9),10,11,12,13),14,15,16)");

    f.assert(7, "add2(3,4)");
    f.assert(1, "sub2(4,3)");
    f.assert(55, "fib(9)");

    f.assert(1, "({ sub_char(7, 3, 3); })");

    f.assert(1, "sub_long(7, 3, 3)");
    f.assert(1, "sub_short(7, 3, 3)");

    f.line("g1 = 3;");
    f.assert(3, "*g1_ptr()");
    f.assert(5, "int_to_char(261)");
    f.assert(261, "first_long(261, 0)");
    f.assert(-5, "div_long(-10, 2)");

    f.assert(1, "bool_fn_add(3)");
    f.assert(0, "bool_fn_sub(3)");
    f.assert(1, "bool_fn_add(-3)");
    f.assert(0, "bool_fn_sub(-3)");
    f.assert(1, "bool_fn_add(0)");
    f.assert(1, "bool_fn_sub(0)");

    f.assert(3, "static_fn()");

    f.finish();
    f.run("function");
}

#[rustfmt::skip]
#[test]
fn test_literal() {
    let mut f = Fixture::new();
    f.main();

    f.assert(97, r#"'a'"#);
    f.assert(10, r#"'\n'"#);
    f.assert(-128, r#"'\x80'"#);

    f.finish();
    f.run("literal");
}

#[test]
fn test_pointer() {
    let mut f = Fixture::new();
    f.main();

    f.assert(3, "({ int x=3; *&x; })");
    f.assert(3, "({ int x=3; int *y=&x; int **z=&y; **z; })");
    f.assert(5, "({ int x=3; int y=5; *(&x+1); })");
    f.assert(3, "({ int x=3; int y=5; *(&y-1); })");
    f.assert(5, "({ int x=3; int y=5; *(&x-(-1)); })");
    f.assert(5, "({ int x=3; int *y=&x; *y=5; x; })");
    f.assert(7, "({ int x=3; int y=5; *(&x+1)=7; y; })");
    f.assert(7, "({ int x=3; int y=5; *(&y-2+1)=7; x; })");
    f.assert(5, "({ int x=3; (&x+2)-&x+3; })");
    f.assert(8, "({ int x, y; x=3; y=5; x+y; })");
    f.assert(8, "({ int x=3, y=5; x+y; })");

    f.assert(3, "({ int x[2]; int *y=&x; *y=3; *x; })");

    f.assert(3, "({ int x[3]; *x=3; *(x+1)=4; *(x+2)=5; *x; })");
    f.assert(4, "({ int x[3]; *x=3; *(x+1)=4; *(x+2)=5; *(x+1); })");
    f.assert(5, "({ int x[3]; *x=3; *(x+1)=4; *(x+2)=5; *(x+2); })");

    f.assert(0, "({ int x[2][3]; int *y=x; *y=0; **x; })");
    f.assert(1, "({ int x[2][3]; int *y=x; *(y+1)=1; *(*x+1); })");
    f.assert(2, "({ int x[2][3]; int *y=x; *(y+2)=2; *(*x+2); })");
    f.assert(3, "({ int x[2][3]; int *y=x; *(y+3)=3; **(x+1); })");
    f.assert(4, "({ int x[2][3]; int *y=x; *(y+4)=4; *(*(x+1)+1); })");
    f.assert(5, "({ int x[2][3]; int *y=x; *(y+5)=5; *(*(x+1)+2); })");

    f.assert(3, "({ int x[3]; *x=3; x[1]=4; x[2]=5; *x; })");
    f.assert(4, "({ int x[3]; *x=3; x[1]=4; x[2]=5; *(x+1); })");
    f.assert(5, "({ int x[3]; *x=3; x[1]=4; x[2]=5; *(x+2); })");
    f.assert(5, "({ int x[3]; *x=3; x[1]=4; x[2]=5; *(x+2); })");
    f.assert(5, "({ int x[3]; *x=3; x[1]=4; 2[x]=5; *(x+2); })");

    f.assert(0, "({ int x[2][3]; int *y=x; y[0]=0; x[0][0]; })");
    f.assert(1, "({ int x[2][3]; int *y=x; y[1]=1; x[0][1]; })");
    f.assert(2, "({ int x[2][3]; int *y=x; y[2]=2; x[0][2]; })");
    f.assert(3, "({ int x[2][3]; int *y=x; y[3]=3; x[1][0]; })");
    f.assert(4, "({ int x[2][3]; int *y=x; y[4]=4; x[1][1]; })");
    f.assert(5, "({ int x[2][3]; int *y=x; y[5]=5; x[1][2]; })");

    f.finish();
    f.run("pointer");
}

#[test]
fn test_sizeof() {
    let mut f = Fixture::new();
    f.main();

    f.assert(1, "sizeof(char)");
    f.assert(2, "sizeof(short)");
    f.assert(2, "sizeof(short int)");
    f.assert(2, "sizeof(int short)");
    f.assert(4, "sizeof(int)");
    f.assert(8, "sizeof(long)");
    f.assert(8, "sizeof(long int)");
    f.assert(8, "sizeof(long int)");
    f.assert(8, "sizeof(char *)");
    f.assert(8, "sizeof(int *)");
    f.assert(8, "sizeof(long *)");
    f.assert(8, "sizeof(int **)");
    f.assert(8, "sizeof(int(*)[4])");
    f.assert(32, "sizeof(int*[4])");
    f.assert(16, "sizeof(int[4])");
    f.assert(48, "sizeof(int[3][4])");
    f.assert(8, "sizeof(struct {int a; int b;})");

    f.assert(8, "sizeof(-10 + (long)5)");
    f.assert(8, "sizeof(-10 - (long)5)");
    f.assert(8, "sizeof(-10 * (long)5)");
    f.assert(8, "sizeof(-10 / (long)5)");
    f.assert(8, "sizeof((long)-10 + 5)");
    f.assert(8, "sizeof((long)-10 - 5)");
    f.assert(8, "sizeof((long)-10 * 5)");
    f.assert(8, "sizeof((long)-10 / 5)");

    f.assert(1, "({ char i; sizeof(++i); })");
    f.assert(1, "({ char i; sizeof(i++); })");

    f.finish();
    f.run("sizeof");
}

#[rustfmt::skip]
#[test]
fn test_string() {
    let mut f = Fixture::new();
    f.main();

    f.assert(0, r#"""[0]"#);
    f.assert(1, r#"sizeof("")"#);
    f.assert(97, r#""abc"[0]"#);
    f.assert(98, r#""abc"[1]"#);
    f.assert(99, r#""abc"[2]"#);
    f.assert(0, r#""abc"[3]"#);
    f.assert(4, r#"sizeof("abc")"#);

    f.assert(7, r#""\a"[0]"#);
    f.assert(8, r#""\b"[0]"#);
    f.assert(9, r#""\t"[0]"#);
    f.assert(10, r#""\n"[0]"#);
    f.assert(11, r#""\v"[0]"#);
    f.assert(12, r#""\f"[0]"#);
    f.assert(13, r#""\r"[0]"#);
    f.assert(27, r#""\e"[0]"#);

    f.assert(106, r#""\j"[0]"#);
    f.assert(107, r#""\k"[0]"#);
    f.assert(108, r#""\l"[0]"#);

    f.assert(7, r#""\ax\ny"[0]"#);
    f.assert(120, r#""\ax\ny"[1]"#);
    f.assert(10, r#""\ax\ny"[2]"#);
    f.assert(121, r#""\ax\ny"[3]"#);

    f.assert(0, r#""\0"[0]"#);
    f.assert(16, r#""\20"[0]"#);
    f.assert(65, r#""\101"[0]"#);
    f.assert(104, r#""\1500"[0]"#);
    f.assert(0, r#""\x00"[0]"#);
    f.assert(119, r#""\x77"[0]"#);

    f.finish();
    f.run("string");
}

#[rustfmt::skip]
#[test]
fn test_struct() {
    let mut f = Fixture::new();
    f.main();

    f.assert(1, "({ struct {int a; int b;} x; x.a=1; x.b=2; x.a; })");
    f.assert(2, "({ struct {int a; int b;} x; x.a=1; x.b=2; x.b; })");
    f.assert(1, "({ struct {char a; int b; char c;} x; x.a=1; x.b=2; x.c=3; x.a; })");
    f.assert(2, "({ struct {char a; int b; char c;} x; x.b=1; x.b=2; x.c=3; x.b; })");
    f.assert(3, "({ struct {char a; int b; char c;} x; x.a=1; x.b=2; x.c=3; x.c; })");

    f.assert(0, "({ struct {char a; char b;} x[3]; char *p=x; p[0]=0; x[0].a; })");
    f.assert(1, "({ struct {char a; char b;} x[3]; char *p=x; p[1]=1; x[0].b; })");
    f.assert(2, "({ struct {char a; char b;} x[3]; char *p=x; p[2]=2; x[1].a; })");
    f.assert(3, "({ struct {char a; char b;} x[3]; char *p=x; p[3]=3; x[1].b; })");

    f.assert(6, "({ struct {char a[3]; char b[5];} x; char *p=&x; x.a[0]=6; p[0]; })");
    f.assert(7, "({ struct {char a[3]; char b[5];} x; char *p=&x; x.b[0]=7; p[3]; })");

    f.assert(6, "({ struct { struct { char b; } a; } x; x.a.b=6; x.a.b; })");

    f.assert(4, "({ struct {int a;} x; sizeof(x); })");
    f.assert(8, "({ struct {int a; int b;} x; sizeof(x); })");
    f.assert(8, "({ struct {int a, b;} x; sizeof(x); })");
    f.assert(12, "({ struct {int a[3];} x; sizeof(x); })");
    f.assert(16, "({ struct {int a;} x[4]; sizeof(x); })");
    f.assert(24, "({ struct {int a[3];} x[2]; sizeof(x); })");
    f.assert(2, "({ struct {char a; char b;} x; sizeof(x); })");
    f.assert(0, "({ struct {} x; sizeof(x); })");
    f.assert(8, "({ struct {char a; int b;} x; sizeof(x); })");
    f.assert(8, "({ struct {int a; char b;} x; sizeof(x); })");

    f.assert(8, "({ struct t {int a; int b;} x; struct t y; sizeof(y); })");
    f.assert(8, "({ struct t {int a; int b;}; struct t y; sizeof(y); })");
    f.assert(2, "({ struct t {char a[2];}; { struct t {char a[4];}; } struct t y; sizeof(y); })");
    f.assert(3, "({ struct t {int x;}; int t=1; struct t y; y.x=2; t+y.x; })");

    f.assert(3, "({ struct t {char a;} x; struct t *y = &x; x.a=3; y->a; })");
    f.assert(3, "({ struct t {char a;} x; struct t *y = &x; y->a=3; x.a; })");

    f.assert(3, "({ struct {int a,b;} x,y; x.a=3; y=x; y.a; })");
    f.assert(7, "({ struct t {int a,b;}; struct t x; x.a=7; struct t y; struct t *z=&y; *z=x; y.a; })");
    f.assert(7, "({ struct t {int a,b;}; struct t x; x.a=7; struct t y, *p=&x, *q=&y; *q=*p; y.a; })");
    f.assert(5, "({ struct t {char a, b;} x, y; x.a=5; y=x; y.a; })");

    f.assert(8, "({ struct t {int a; int b;}; struct t y; sizeof(y); })");
    f.assert(8, "({ struct t {int a; int b;} x; struct t y; sizeof(y); })");

    f.assert(16, "({ struct {char a; long b;} x; sizeof(x); })");
    f.assert(4, "({ struct {char a; short b;} x; sizeof(x); })");

    f.finish();
    f.run("struct");
}

#[rustfmt::skip]
#[test]
fn test_typedef() {
    let mut f = Fixture::new();
    f.line("typedef int MyInt, MyInt2[4];");
    f.line("typedef int;");
    f.main();

    f.assert(1, "({ typedef int t; t x=1; x; })");
    f.assert(1, "({ typedef struct {int a;} t; t x; x.a=1; x.a; })");
    f.assert(2, "({ typedef struct {int a;} t; { typedef int t; } t x; x.a=2; x.a; })");
    f.assert(3, "({ MyInt x=3; x; })");
    f.assert(16," ({ MyInt2 x; sizeof(x); })");

    f.finish();
    f.run("typedef");
}

#[rustfmt::skip]
#[test]
fn test_union() {
    let mut f = Fixture::new();
    f.main();

    f.assert(8, "({ union { int a; char b[6]; } x; sizeof(x); })");
    f.assert(3, "({ union { int a; char b[4]; } x; x.a = 515; x.b[0]; })");
    f.assert(2, "({ union { int a; char b[4]; } x; x.a = 515; x.b[1]; })");
    f.assert(0, "({ union { int a; char b[4]; } x; x.a = 515; x.b[2]; })");
    f.assert(0, "({ union { int a; char b[4]; } x; x.a = 515; x.b[3]; })");

    f.assert(3, "({ union {int a,b;} x,y; x.a=3; y.a=5; y=x; y.a; })");
    f.assert(3, "({ union {struct {int a,b;} c;} x,y; x.c.b=3; y.c.b=5; y=x; y.c.b; })");

    f.finish();
    f.run("union");
}

#[rustfmt::skip]
#[test]
fn test_usualconv() {
    let mut f = Fixture::new();
    f.main();

    f.assert("(long)-5", "-10 + (long)5");
    f.assert("(long)-15", "-10 - (long)5");
    f.assert("(long)-50", "-10 * (long)5");
    f.assert("(long)-2", "-10 / (long)5");

    f.assert(1, "-2 < (long)-1");
    f.assert(1, "-2 <= (long)-1");
    f.assert(0, "-2 > (long)-1");
    f.assert(0, "-2 >= (long)-1");

    f.assert(1, "(long)-2 < -1");
    f.assert(1, "(long)-2 <= -1");
    f.assert(0, "(long)-2 > -1");
    f.assert(0, "(long)-2 >= -1");

    f.assert(0, "2147483647 + 2147483647 + 2");
    f.assert("(long)-1", "({ long x; x=-1; x; })");

    f.assert(1, "({ char x[3]; x[0]=0; x[1]=1; x[2]=2; char *y=x+1; y[0]; })");
    f.assert(0, "({ char x[3]; x[0]=0; x[1]=1; x[2]=2; char *y=x+1; y[-1]; })");
    f.assert(5, "({ struct t {char a;} x, y; x.a=5; y=x; y.a; })");

    f.finish();
    f.run("usualconv");
}

#[rustfmt::skip]
#[test]
fn test_variable() {
    let mut f = Fixture::new();
    f.line("int g1, g2[4];");
    f.main();

    f.assert(3, "({ int a; a=3; a; })");
    f.assert(3, "({ int a=3; a; })");
    f.assert(8, "({ int a=3; int z=5; a+z; })");

    f.assert(3, "({ int a=3; a; })");
    f.assert(8, "({ int a=3; int z=5; a+z; })");
    f.assert(6, "({ int a; int b; a=b=3; a+b; })");
    f.assert(3, "({ int foo=3; foo; })");
    f.assert(8, "({ int foo123=3; int bar=5; foo123+bar; })");

    f.assert(4, "({ int x; sizeof(x); })");
    f.assert(4, "({ int x; sizeof x; })");
    f.assert(8, "({ int *x; sizeof(x); })");
    f.assert(16, "({ int x[4]; sizeof(x); })");
    f.assert(48, "({ int x[3][4]; sizeof(x); })");
    f.assert(16, "({ int x[3][4]; sizeof(*x); })");
    f.assert(4, "({ int x[3][4]; sizeof(**x); })");
    f.assert(5, "({ int x[3][4]; sizeof(**x) + 1; })");
    f.assert(5, "({ int x[3][4]; sizeof **x + 1; })");
    f.assert(4, "({ int x[3][4]; sizeof(**x + 1); })");
    f.assert(4, "({ int x=1; sizeof(x=2); })");
    f.assert(1, "({ int x=1; sizeof(x=2); x; })");

    f.assert(0, "g1");
    f.assert(3, "({ g1=3; g1; })");
    f.assert(0, "({ g2[0]=0; g2[1]=1; g2[2]=2; g2[3]=3; g2[0]; })");
    f.assert(1, "({ g2[0]=0; g2[1]=1; g2[2]=2; g2[3]=3; g2[1]; })");
    f.assert(2, "({ g2[0]=0; g2[1]=1; g2[2]=2; g2[3]=3; g2[2]; })");
    f.assert(3, "({ g2[0]=0; g2[1]=1; g2[2]=2; g2[3]=3; g2[3]; })");

    f.assert(4, "sizeof(g1)");
    f.assert(16, "sizeof(g2)");

    f.assert(1, "({ char x=1; x; })");
    f.assert(1, "({ char x=1; char y=2; x; })");
    f.assert(2, "({ char x=1; char y=2; y; })");

    f.assert(1, "({ char x; sizeof(x); })");
    f.assert(10, "({ char x[10]; sizeof(x); })");

    f.assert(2, "({ int x=2; { int x=3; } x; })");
    f.assert(2, "({ int x=2; { int x=3; } int y=4; x; })");
    f.assert(3, "({ int x=2; { x=3; } x; })");

    f.assert(7, "({ int x; int y; char z; char *a=&y; char *b=&z; b-a; })");
    f.assert(1, "({ int x; char y; int z; char *a=&y; char *b=&z; b-a; })");

    f.assert(8, "({ long x; sizeof(x); })");
    f.assert(2, "({ short x; sizeof(x); })");

    f.assert(24, "({ char *x[3]; sizeof(x); })");
    f.assert(8, "({ char (*x)[3]; sizeof(x); })");
    f.assert(1, "({ char (x); sizeof(x); })");
    f.assert(3, "({ char (x)[3]; sizeof(x); })");
    f.assert(12, "({ char (x[3])[4]; sizeof(x); })");
    f.assert(4, "({ char (x[3])[4]; sizeof(x[0]); })");
    f.assert(3, "({ char *x[3]; char y; x[0]=&y; y=3; x[0][0]; })");
    f.assert(4, "({ char x[3]; char (*y)[3]=x; y[0][0]=4; y[0][0]; })");

    f.line("{ void *x; }");

    f.finish();
    f.run("variable");
}

#[test]
fn test_help_flag() {
    let output = Command::chacc()
        .arg("--help")
        .run_checked("running with --help flag");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("chacc"));
}

#[test]
fn test_output_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");
    let input = tmp.path().join("input.c");
    std::fs::write(&input, "int main() { return 0; }\n").expect("failed to write input f");

    let asm = tmp.path().join("out.s");
    Command::chacc()
        .arg("-o")
        .arg(&asm)
        .arg(&input)
        .run_checked("compiling with -o flag");
    assert!(asm.is_file(), "expected {} to be created", asm.display());
}
