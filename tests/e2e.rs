use std::ffi::OsString;
use std::io::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rustc_hash::FxHashMap;
use tempfile::{TempDir, tempdir};

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn create_c(dir: &Path) -> Result<PathBuf> {
    let main = dir.join("main.c");
    std::fs::write(&main, "int main() { return 42; }\n")?;
    Ok(main)
}

fn create_multi_c(dir: &Path) -> Result<Vec<PathBuf>> {
    let foo = dir.join("foo.c");
    let bar = dir.join("bar.c");
    let baz = dir.join("baz.c");

    std::fs::write(
        &foo,
        "int bar(); int baz(); int main() { return bar() + baz(); }\n",
    )?;
    std::fs::write(&bar, "int bar() { return 30; }")?;
    std::fs::write(&baz, "int baz() { return 12; }")?;

    Ok(vec![foo, bar, baz])
}

trait CommandExt {
    fn cc(dir: impl AsRef<Path>) -> Command {
        let cc = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
        let mut command = Command::new(cc);
        command.current_dir(dir);
        command
    }

    fn chacc(dir: impl AsRef<Path>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_chacc"));
        command.arg("-###");
        command.current_dir(dir);
        command
    }

    fn run_checked(&mut self, what: &str, code: Option<i32>) -> Output;
}

impl CommandExt for Command {
    fn run_checked(&mut self, what: &str, code: Option<i32>) -> Output {
        let output = self
            .output()
            .unwrap_or_else(|err| panic!("{what} failed to start: {err}"));

        eprintln!(
            "command: {self:?}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        if let Some(code) = code {
            assert!(!output.status.success(), "{what} didn't fail");
            assert_eq!(
                output.status.code(),
                Some(code),
                "{what} got unexpected exit code"
            );
        } else {
            assert!(output.status.success(), "{what} failed");
        }

        output
    }
}

#[derive(Debug)]
struct Fixture {
    source: String,
    includes: FxHashMap<&'static str, &'static str>,
    tmp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            source: String::new(),
            includes: FxHashMap::default(),
            tmp: tempdir().expect("failed to create temporary directory"),
        };
        f.line("int assert(int expected, int actual, char *code);");
        f.line("int strcmp(char *lhs, char *rhs);");
        f.line("int memcmp(char *lhs, char *rhs, int n);");
        f.line("void exit(int code);");
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
        self.line("  int __n_failed = 0;");
    }

    fn assert<E>(&mut self, expected: E, actual: &str)
    where
        E: std::fmt::Display,
    {
        let line = format!("  __n_failed += assert(({expected}), ({actual}), ({actual:?}));");
        self.line(&line);
    }

    fn finish(&mut self) {
        self.line("  return __n_failed != 0;");
        self.line("}");
    }

    fn run(&self, stem: &str) {
        let tests_dir = tests_dir();
        let path = self.tmp.path();

        let source = path.join(format!("{stem}.c"));
        let obj = path.join(format!("{stem}.o"));
        let exe = path.join(stem);

        std::fs::write(&source, &self.source).expect("failed to write fixture");

        for (name, content) in &self.includes {
            let path = path.join(name);
            std::fs::write(&path, content).expect("failed to write fixture include");
        }

        Command::chacc(path)
            .arg("-c")
            .arg("-o")
            .arg(&obj)
            .arg(&source)
            .run_checked(&format!("compiling {}", source.display()), None);

        Command::cc(path)
            .arg("-o")
            .arg(&exe)
            .arg(&obj)
            .arg(tests_dir.join("test.c"))
            .run_checked(&format!("linking {}", source.display()), None);

        Command::new(&exe).run_checked(
            &format!("running {}", source.file_name().unwrap().to_string_lossy()),
            None,
        );
    }
}

#[rustfmt::skip]
#[test]
fn test_alignof() {
    let mut f = Fixture::new();
    f.line("int _Alignas(512) g1;");
    f.line("int _Alignas(512) g2;");
    f.line("char g3;");
    f.line("int g4;");
    f.line("long g5;");
    f.line("char g6;");
    f.main();

    f.assert(1, "_Alignof(char)");
    f.assert(2, "_Alignof(short)");
    f.assert(4, "_Alignof(int)");
    f.assert(8, "_Alignof(long)");
    f.assert(8, "_Alignof(long long)");
    f.assert(1, "_Alignof(char[3])");
    f.assert(4, "_Alignof(int[3])");
    f.assert(1, "_Alignof(struct {char a; char b;}[2])");
    f.assert(8, "_Alignof(struct {char a; long b;}[2])");

    f.assert(1, "({ _Alignas(char) char x, y; &y-&x; })");
    f.assert(8, "({ _Alignas(long) char x, y; &y-&x; })");
    f.assert(32, "({ _Alignas(32) char x, y; &y-&x; })");
    f.assert(32, "({ _Alignas(32) int *x, *y; ((char *)&y)-((char *)&x); })");
    f.assert(16, "({ struct { _Alignas(16) char x, y; } a; &a.y-&a.x; })");
    f.assert(8, "({ struct T { _Alignas(8) char a; }; _Alignof(struct T); })");

    f.assert(0, "(long)(char *)&g1 % 512");
    f.assert(0, "(long)(char *)&g2 % 512");
    f.assert(0, "(long)(char *)&g4 % 4");
    f.assert(0, "(long)(char *)&g5 % 8");

    f.assert(1, "({ char x; _Alignof(x); })");
    f.assert(4, "({ int x; _Alignof(x); })");
    f.assert(1, "({ char x; _Alignof x; })");
    f.assert(4, "({ int x; _Alignof x; })");

    f.assert(1, "_Alignof(char) << 31 >> 31");
    f.assert(1, "_Alignof(char) << 63 >> 63");
    f.assert(1, "({ char x; _Alignof(x) << 63 >> 63; })");

    f.finish();
    f.run("alignof");
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

    f.assert(0, "!1");
    f.assert(0, "!2");
    f.assert(1, "!0");
    f.assert(1, "!(char)0");
    f.assert(0, "!(long)3");
    f.assert(4, "sizeof(!(char)0)");
    f.assert(4, "sizeof(!(long)0)");

    f.assert(-1, "~0");
    f.assert(0, "~-1");

    f.assert(5, "17%6");
    f.assert(5, "((long)17)%6");
    f.assert(2, "({ int i=10; i%=4; i; })");
    f.assert(2, "({ long i=10; i%=4; i; })");

    f.assert(0, "0&1");
    f.assert(1, "3&1");
    f.assert(3, "7&3");
    f.assert(10, "-1&10");

    f.assert(1, "0|1");
    f.assert(0b10011, "0b10000|0b00011");

    f.assert(0, "0^0");
    f.assert(0, "0b1111^0b1111");
    f.assert(0b110100, "0b111000^0b001100");

    f.assert(2, "({ int i=6; i&=3; i; })");
    f.assert(7, "({ int i=6; i|=3; i; })");
    f.assert(10, "({ int i=15; i^=5; i; })");

    f.assert(1, "1<<0");
    f.assert(8, "1<<3");
    f.assert(10, "5<<1");
    f.assert(2, "5>>1");
    f.assert(-1, "-1>>1");
    f.assert(1, "({ int i=1; i<<=0; i; })");
    f.assert(8, "({ int i=1; i<<=3; i; })");
    f.assert(10, "({ int i=5; i<<=1; i; })");
    f.assert(2, "({ int i=5; i>>=1; i; })");
    f.assert(-1, "({ int i=-1; i>>=1; i; })");

    f.assert(2, "0?1:2");
    f.assert(1, "1?1:2");
    f.assert(-1, "0?-2:-1");
    f.assert(-2, "1?-2:-1");
    f.assert(4, "sizeof(0?1:2)");
    f.assert(8, "sizeof(0?(long)1:(long)2)");
    f.assert(-1, "0?(long)-2:-1");
    f.assert(-1, "0?-2:(long)-1");
    f.assert(-2, "1?(long)-2:-1");
    f.assert(-2, "1?-2:(long)-1");

    f.line("1 ? -2 : (void)-1;");

    f.assert(20, "({ int x; int *p=&x; p+20-p; })");
    f.assert(1, "({ int x; int *p=&x; p+20-p>0; })");
    f.assert(-20, "({ int x; int *p=&x; p-20-p; })");
    f.assert(1, "({ int x; int *p=&x; p-20-p<0; })");

    f.assert(15, "(char *)0xffffffffffffffff - (char *)0xfffffffffffffff0");
    f.assert(-15, "(char *)0xfffffffffffffff0 - (char *)0xffffffffffffffff");
    f.assert(1, "(void *)0xffffffffffffffff > (void *)0");

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

    f.assert(-1, "(char)255");
    f.assert(-1, "(signed char)255");
    f.assert(255, "(unsigned char)255");
    f.assert(-1, "(short)65535");
    f.assert(65535, "(unsigned short)65535");
    f.assert(-1, "(int)0xffffffff");
    f.assert(1, "(unsigned)0xffffffff > 0");

    f.assert(1, "-1<1");
    f.assert(0, "-1<(unsigned)1");
    f.assert(254, "(char)127+(char)127");
    f.assert(65534, "(short)32767+(short)32767");
    f.assert(-1, "-1>>1");
    f.assert(1, "(unsigned long)-1 > 0");
    f.assert(2147483647, "((unsigned)-1)>>1");
    f.assert(-50, "(-100)/2");
    f.assert(1, "((unsigned)-100)/2 == 2147483598");
    f.assert(1, "((unsigned long)-100)/2 == 9223372036854775758");
    f.assert(0, "((long)-1)/(unsigned)100");
    f.assert(-2, "(-100)%7");
    f.assert(2, "((unsigned)-100)%7");
    f.assert(6, "((unsigned long)-100)%9");

    f.assert(65535, "(int)(unsigned short)65535");
    f.assert(65535, "({ unsigned short x = 65535; x; })");
    f.assert(65535, "({ unsigned short x = 65535; (int)x; })");

    f.assert(-1, "({ typedef short T; T x = 65535; (int)x; })");
    f.assert(65535, "({ typedef unsigned short T; T x = 65535; (int)x; })");

    f.assert(0, "(_Bool)0.0");
    f.assert(1, "(_Bool)0.1");
    f.assert(3, "(char)3.0");
    f.assert(1000, "(short)1000.3");
    f.assert(3, "(int)3.99");
    f.assert(2000000000000000i64, "(long)2e15");
    f.assert(3, "(float)3.5");
    f.assert(5, "(double)(float)5.5");
    f.assert(3, "(float)3");
    f.assert(3, "(double)3");
    f.assert(3, "(float)3L");
    f.assert(3, "(double)3L");

    f.finish();
    f.run("cast");
}

#[rustfmt::skip]
#[test]
fn test_compat() {
    let mut f = Fixture::new();
    f.line("_Noreturn int ignored_global;");
    f.line("_Noreturn noreturn_fn(int restrict x) { exit(0); }");
    f.line("void funcy_type(int arg[restrict static 3]) {}");
    f.main();

    f.line("{ _Noreturn x; }");
    f.line("{ volatile x; }");
    f.line("{ int volatile x; }");
    f.line("{ volatile int x; }");
    f.line("{ volatile int volatile volatile x; }");
    f.line("{ int volatile * volatile volatile x; }");
    f.line("{ auto ** restrict __restrict __restrict__ const volatile *x; }");

    f.finish();
    f.run("compat");
}

#[rustfmt::skip]
#[test]
fn test_compound_literal() {
    let mut f = Fixture::new();
    f.line("typedef struct Tree {int val; struct Tree *lhs; struct Tree *rhs;} Tree;");
    f.line("Tree *tree = &(Tree){1, &(Tree){2, &(Tree){3,0,0}, &(Tree){4,0,0}}, 0};");
    f.main();

    f.assert(1, "(int){1}");
    f.assert(2, "((int[]){0,1,2})[2]");
    f.assert("'a'", "((struct {char a; int b;}){'a', 3}).a");
    f.assert(3, "({ int x=3; (int){x}; })");
    f.line("(int){3} = 5;");

    f.assert(1, "tree->val");
    f.assert(2, "tree->lhs->val");
    f.assert(3, "tree->lhs->lhs->val");
    f.assert(4, "tree->lhs->rhs->val");

    f.finish();
    f.run("compound_literal");
}

#[rustfmt::skip]
#[test]
fn test_const() {
    let mut f = Fixture::new();
    f.main();

    f.line("{ const x; }");
    f.line("{ int const x; }");
    f.line("{ const int x; }");
    f.line("{ const int const const x; }");
    f.assert(5, "({ const x = 5; x; })");
    f.assert(8, "({ const x = 8; int *const y=&x; *y; })");
    f.assert(6, "({ const x = 6; *(const * const)&x; })");

    f.finish();
    f.run("const");
}

#[rustfmt::skip]
#[test]
fn test_constexpr() {
    let mut f = Fixture::new();
    f.line("float g40 = 1.5;");
    f.line("double g41 = 0.0 ? 55 : (0, 1 + 1 * 5.0 / 2 * (double)2 * (int)2.0);");
    f.main();

    f.assert(10, "({ enum { ten=1+2+3+4 }; ten; })");
    f.assert(1, "({ int i=0; switch(3) { case 5-2+0*3: i++; } i; })");
    f.assert(8, "({ int x[1+1]; sizeof(x); })");
    f.assert(6, "({ char x[8-2]; sizeof(x); })");
    f.assert(6, "({ char x[2*3]; sizeof(x); })");
    f.assert(3, "({ char x[12/4]; sizeof(x); })");
    f.assert(2, "({ char x[12%10]; sizeof(x); })");
    f.assert(0b100, "({ char x[0b110&0b101]; sizeof(x); })");
    f.assert(0b111, "({ char x[0b110|0b101]; sizeof(x); })");
    f.assert(0b110, "({ char x[0b111^0b001]; sizeof(x); })");
    f.assert(4, "({ char x[1<<2]; sizeof(x); })");
    f.assert(2, "({ char x[4>>1]; sizeof(x); })");
    f.assert(2, "({ char x[(1==1)+1]; sizeof(x); })");
    f.assert(1, "({ char x[(1!=1)+1]; sizeof(x); })");
    f.assert(1, "({ char x[(1<1)+1]; sizeof(x); })");
    f.assert(2, "({ char x[(1<=1)+1]; sizeof(x); })");
    f.assert(2, "({ char x[1?2:3]; sizeof(x); })");
    f.assert(3, "({ char x[0?2:3]; sizeof(x); })");
    f.assert(3, "({ char x[(1,3)]; sizeof(x); })");
    f.assert(2, "({ char x[!0+1]; sizeof(x); })");
    f.assert(1, "({ char x[!1+1]; sizeof(x); })");
    f.assert(2, "({ char x[~-3]; sizeof(x); })");
    f.assert(2, "({ char x[(5||6)+1]; sizeof(x); })");
    f.assert(1, "({ char x[(0||0)+1]; sizeof(x); })");
    f.assert(2, "({ char x[(1&&1)+1]; sizeof(x); })");
    f.assert(1, "({ char x[(1&&0)+1]; sizeof(x); })");
    f.assert(3, "({ char x[(int)3]; sizeof(x); })");
    f.assert(15, "({ char x[(char)0xffffff0f]; sizeof(x); })");
    f.assert(0x10f, "({ char x[(short)0xffff010f]; sizeof(x); })");
    f.assert(4, "({ char x[(int)0xfffffffffff+5]; sizeof(x); })");
    f.assert(8, "({ char x[(int*)0+2]; sizeof(x); })");
    f.assert(12, "({ char x[(int*)16-1]; sizeof(x); })");
    f.assert(3, "({ char x[(int*)16-(int*)4]; sizeof(x); })");

    f.assert(4, "({ char x[(-1>>31)+5]; sizeof(x); })");
    f.assert(255, "({ char x[(unsigned char)0xffffffff]; sizeof(x); })");
    f.assert(0x800f, "({ char x[(unsigned short)0xffff800f]; sizeof(x); })");
    f.assert(1, "({ char x[(unsigned int)0xfffffffffff>>31]; sizeof(x); })");
    f.assert(1, "({ char x[(long)-1/((long)1<<62)+1]; sizeof(x); })");
    f.assert(4, "({ char x[(unsigned long)-1/((long)1<<62)+1]; sizeof(x); })");
    f.assert(1, "({ char x[(unsigned)1<-1]; sizeof(x); })");
    f.assert(1, "({ char x[(unsigned)1<=-1]; sizeof(x); })");

    f.assert(1, "g40==1.5");
    f.assert(1, "g41==11");

    f.finish();
    f.run("constexpr");
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

    f.assert(1, "0||1");
    f.assert(1, "0||(2-2)||5");
    f.assert(0, "0||0");
    f.assert(0, "0||(2-2)");

    f.assert(0, "0&&1");
    f.assert(0, "(2-2)&&5");
    f.assert(1, "1&&5");

    f.assert(3, "({ int i=0; goto a; a: i++; b: i++; c: i++; i; })");
    f.assert(2, "({ int i=0; goto e; d: i++; e: i++; f: i++; i; })");
    f.assert(1, "({ int i=0; goto i; g: i++; h: i++; i: i++; i; })");

    f.assert(1, "({ typedef int foo; goto foo; foo:; 1; })");

    f.assert(3, "({ int i=0; for(;i<10;i++) { if (i == 3) break; } i; })");
    f.assert(4, "({ int i=0; while (1) { if (i++ == 3) break; } i; })");
    f.assert(3, "({ int i=0; for(;i<10;i++) { for (;;) break; if (i == 3) break; } i; })");
    f.assert(4, "({ int i=0; while (1) { while(1) break; if (i++ == 3) break; } i; })");

    f.assert(10, "({ int i=0; int j=0; for (;i<10;i++) { if (i>5) continue; j++; } i; })");
    f.assert(6, "({ int i=0; int j=0; for (;i<10;i++) { if (i>5) continue; j++; } j; })");
    f.assert(10, "({ int i=0; int j=0; for(;!i;) { for (;j!=10;j++) continue; break; } j; })");
    f.assert(11, "({ int i=0; int j=0; while (i++<10) { if (i>5) continue; j++; } i; })");
    f.assert(5, "({ int i=0; int j=0; while (i++<10) { if (i>5) continue; j++; } j; })");
    f.assert(11, "({ int i=0; int j=0; while(!i) { while (j++!=10) continue; break; } j; })");

    f.assert(5, "({ int i=0; switch(0) { case 0:i=5;break; case 1:i=6;break; case 2:i=7;break; } i; })");
    f.assert(6, "({ int i=0; switch(1) { case 0:i=5;break; case 1:i=6;break; case 2:i=7;break; } i; })");
    f.assert(7, "({ int i=0; switch(2) { case 0:i=5;break; case 1:i=6;break; case 2:i=7;break; } i; })");
    f.assert(0, "({ int i=0; switch(3) { case 0:i=5;break; case 1:i=6;break; case 2:i=7;break; } i; })");
    f.assert(5, "({ int i=0; switch(0) { case 0:i=5;break; default:i=7; } i; })");
    f.assert(7, "({ int i=0; switch(1) { case 0:i=5;break; default:i=7; } i; })");
    f.assert(2, "({ int i=0; switch(1) { case 0: 0; case 1: 0; case 2: 0; i=2; } i; })");
    f.assert(0, "({ int i=0; switch(3) { case 0: 0; case 1: 0; case 2: 0; i=2; } i; })");

    f.assert(3, "({ int i=0; switch(-1) { case 0xffffffff: i=3; break; } i; })");

    f.assert(7, "({ int i=0; int j=0; do { j++; } while (i++ < 6); j; })");
    f.assert(4, "({ int i=0; int j=0; int k=0; do { if (++j > 3) break; continue; k++; } while (1); j; })");

    f.assert(0, "0.0 && 0.0");
    f.assert(0, "0.0 && 0.1");
    f.assert(0, "0.3 && 0.0");
    f.assert(1, "0.3 && 0.5");
    f.assert(0, "0.0 || 0.0");
    f.assert(1, "0.0 || 0.1");
    f.assert(1, "0.3 || 0.0");
    f.assert(1, "0.3 || 0.5");
    f.assert(5, "({ int x; if (0.0) x=3; else x=5; x; })");
    f.assert(3, "({ int x; if (0.1) x=3; else x=5; x; })");
    f.assert(5, "({ int x=5; if (0.0) x=3; x; })");
    f.assert(3, "({ int x=5; if (0.1) x=3; x; })");
    f.assert(10, "({ double i=10.0; int j=0; for (; i; i--, j++); j; })");
    f.assert(10, "({ double i=10.0; int j=0; do j++; while(--i); j; })");

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
fn test_extern() {
    let mut f = Fixture::new();
    f.line("extern int ext1;");
    f.line("extern int *ext2;");
    f.main();

    f.assert(5, "ext1");
    f.assert(5, "*ext2");

    f.line("extern int ext3;");
    f.assert(7, "ext3");

    f.line("int ext_fn1(int x);");
    f.assert(5, "ext_fn1(5)");

    f.line("extern int ext_fn2(int x);");
    f.assert(8, "ext_fn2(8)");

    f.finish();
    f.run("extern");
}

#[rustfmt::skip]
#[test]
fn test_float() {
    let mut f = Fixture::new();
    f.main();

    f.assert(35, "(float)(char)35");
    f.assert(35, "(float)(short)35");
    f.assert(35, "(float)(int)35");
    f.assert(35, "(float)(long)35");
    f.assert(35, "(float)(unsigned char)35");
    f.assert(35, "(float)(unsigned short)35");
    f.assert(35, "(float)(unsigned int)35");
    f.assert(35, "(float)(unsigned long)35");

    f.assert(35, "(double)(char)35");
    f.assert(35, "(double)(short)35");
    f.assert(35, "(double)(int)35");
    f.assert(35, "(double)(long)35");
    f.assert(35, "(double)(unsigned char)35");
    f.assert(35, "(double)(unsigned short)35");
    f.assert(35, "(double)(unsigned int)35");
    f.assert(35, "(double)(unsigned long)35");

    f.assert(35, "(char)(float)35");
    f.assert(35, "(short)(float)35");
    f.assert(35, "(int)(float)35");
    f.assert(35, "(long)(float)35");
    f.assert(35, "(unsigned char)(float)35");
    f.assert(35, "(unsigned short)(float)35");
    f.assert(35, "(unsigned int)(float)35");
    f.assert(35, "(unsigned long)(float)35");

    f.assert(35, "(char)(double)35");
    f.assert(35, "(short)(double)35");
    f.assert(35, "(int)(double)35");
    f.assert(35, "(long)(double)35");
    f.assert(35, "(unsigned char)(double)35");
    f.assert(35, "(unsigned short)(double)35");
    f.assert(35, "(unsigned int)(double)35");
    f.assert(35, "(unsigned long)(double)35");

    f.assert(-2147483648, "(double)(unsigned long)(long)-1");

    f.assert(1, "2e3==2e3");
    f.assert(0, "2e3==2e5");
    f.assert(1, "2.0==2");
    f.assert(0, "5.1<5");
    f.assert(0, "5.0<5");
    f.assert(1, "4.9<5");
    f.assert(0, "5.1<=5");
    f.assert(1, "5.0<=5");
    f.assert(1, "4.9<=5");

    f.assert(1, "2e3f==2e3");
    f.assert(0, "2e3f==2e5");
    f.assert(1, "2.0f==2");
    f.assert(0, "5.1f<5");
    f.assert(0, "5.0f<5");
    f.assert(1, "4.9f<5");
    f.assert(0, "5.1f<=5");
    f.assert(1, "5.0f<=5");
    f.assert(1, "4.9f<=5");

    f.assert(6, "2.3+3.8");
    f.assert(-1, "2.3-3.8");
    f.assert(-3, "-3.8");
    f.assert(13, "3.3*4");
    f.assert(2, "5.0/2");

    f.assert(6, "2.3f+3.8f");
    f.assert(6, "2.3f+3.8");
    f.assert(-1, "2.3f-3.8");
    f.assert(-3, "-3.8f");
    f.assert(13, "3.3f*4");
    f.assert(2, "5.0f/2");

    f.assert(0, "0.0/0.0 == 0.0/0.0");
    f.assert(1, "0.0/0.0 != 0.0/0.0");

    f.assert(0, "0.0/0.0 < 0");
    f.assert(0, "0.0/0.0 <= 0");
    f.assert(0, "0.0/0.0 > 0");
    f.assert(0, "0.0/0.0 >= 0");

    f.assert(0, "!3.");
    f.assert(1, "!0.");
    f.assert(0, "!3.f");
    f.assert(1, "!0.f");

    f.assert(5, "0.0 ? 3 : 5");
    f.assert(3, "1.2 ? 3 : 5");

    f.finish();
    f.run("float");
}

#[rustfmt::skip]
#[test]
fn test_function() {
    let mut f = Fixture::new();

    f.line("int ret3(void) { return 3; return 5; }");
    f.line("int add2(int x, int y) { return x + y; }");
    f.line("int sub2(int x, int y) { return x - y; }");
    f.line("int add6(int a, int b, int c, int d, int e, int f) { return a + b + c + d + e + f; }");
    f.line("int addx(int *x, int y) { return *x + y; }");
    f.line("int sub_char(char a, char b, char c) { return a - b - c; }");
    f.line("int fib(int x) { if (x<=1) return 1; return fib(x-1) + fib(x-2); }");
    f.line("int sub_long(long a, long b, long c) { return a - b - c; }");
    f.line("int sub_short(short a, short b, short c) { return a - b - c; }");
    f.line("int g1;");
    f.line("int *g1_ptr(void) { return &g1; }");
    f.line("char int_to_char(int x) { return x; }");
    f.line("long first_long(long a, char b) { return a; }");
    f.line("int div_long(long a, long b) { return a / b; }");
    f.line("_Bool bool_fn_add(_Bool x) { return x + 1; }");
    f.line("_Bool bool_fn_sub(_Bool x) { return x - 1; }");
    f.line("static int static_fn(void) { return 3; }");
    f.line("int param_decay(int x[]) { return x[0]; }");
    f.line("int counter() { static int i; static int j = 1+1; return i++ + j++; }");
    f.line("void ret_none() { return; }");
    f.line("_Bool false_fn();");
    f.line("_Bool true_fn();");
    f.line("char char_fn();");
    f.line("short short_fn();");
    f.line("unsigned char uchar_fn();");
    f.line("unsigned short ushort_fn();");
    f.line("signed char schar_fn();");
    f.line("signed short sshort_fn();");

    f.line("typedef struct {int gp_offset; int fp_offset; void *overflow_arg_area; void *reg_save_area;} __va_elem;");
    f.line("typedef __va_elem va_list[1];");
    f.line("int add_all(int n, ...);");
    f.line("int sprintf(char *buf, char *fmt, ...);");
    f.line("int vsprintf(char *buf, char *fmt, va_list ap);");
    f.line("void fmt(char *buf, char *fmt, ...) { va_list ap; *ap = *(__va_elem *)__va_area__; vsprintf(buf, fmt, ap); }");

    f.line("double add_double(double x, double y);");
    f.line("float add_float(float x, float y);");
    f.line("float add_float3(float x, float y, float z) { return x + y + z; }");
    f.line("double add_double3(double x, double y, double z) { return x + y + z; }");
    f.line("int (*fnptr(int (*fn)(int n, ...)))(int, ...) { return fn; }");
    f.line("int param_decay2(int x()) { return x(); }");
    f.line("char *func_fn(void) { return __func__; }");
    f.line("char *function_fn(void) { return __FUNCTION__; }");
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

    f.assert(3, "({ int x[2]; x[0]=3; param_decay(x); })");

    f.assert(2, "counter()");
    f.assert(4, "counter()");
    f.assert(6, "counter()");

    f.line("ret_none();");

    f.assert(1, "true_fn()");
    f.assert(0, "false_fn()");
    f.assert(3, "char_fn()");
    f.assert(5, "short_fn()");
    f.assert(251, "uchar_fn()");
    f.assert(65528, "ushort_fn()");
    f.assert(-5, "schar_fn()");
    f.assert(-8, "sshort_fn()");

    f.assert(6, "add_all(3,1,2,3)");
    f.assert(5, "add_all(4,1,2,3,-1)");

    f.assert(0, r#"({ char buf[100]; sprintf(buf, "%d %d %s", 1, 2, "foo"); strcmp("1 2 foo", buf); })"#);
    f.assert(0, r#"({ char buf[100]; fmt(buf, "%d %d %s", 1, 2, "foo"); strcmp("1 2 foo", buf); })"#);

    f.assert(6, "add_float(2.3, 3.8)");
    f.assert(6, "add_double(2.3, 3.8)");

    f.assert(7, "add_float3(2.5, 2.5, 2.5)");
    f.assert(7, "add_double3(2.5, 2.5, 2.5)");

    f.assert(0, r#"({ char buf[100]; float x = 3.5; sprintf(buf, "%.1f", x); strcmp(buf, "3.5"); })"#);
    f.assert(0, r#"({ char buf[100]; float x = 3.5; fmt(buf, "%.1f", x); strcmp(buf, "3.5"); })"#);

    f.assert(5, "(add2)(2,3)");
    f.assert(5, "(&add2)(2,3)");
    f.assert(7, "({ int (*fn)(int,int) = add2; fn(2,5); })");
    f.assert(6, "fnptr(add_all)(3, 1, 2, 3)");

    f.assert(3, "param_decay2(ret3)");

    f.assert(5, "sizeof(__func__)");
    f.assert(0, "strcmp(\"main\", __func__)");
    f.assert(0, "strcmp(\"func_fn\", func_fn())");
    f.assert(0, "strcmp(\"main\", __FUNCTION__)");
    f.assert(0, "strcmp(\"function_fn\", function_fn())");

    f.finish();
    f.run("function");
}

#[rustfmt::skip]
#[test]
fn test_initializer() {
    let mut f = Fixture::new();
    f.line("char g3 = 3;");
    f.line("short g4 = 4;");
    f.line("int g5 = 5;");
    f.line("long g6 = 6;");
    f.line("int g9[3] = {0, 1, 2};");
    f.line("struct {char a; int b;} g11[2] = {{1, 2}, {3, 4}};");
    f.line("struct {int a[2];} g12[2] = {{{1, 2}}};");
    f.line("union { int a; char b[8]; } g13[2] = {0x01020304, 0x05060708};");
    f.line(r#"char g17[] = "foobar";"#);
    f.line(r#"char g18[10] = "foobar";"#);
    f.line(r#"char g19[3] = "foobar";"#);
    f.line("char *g20 = g17+0;");
    f.line("char *g21 = g17+3;");
    f.line("char *g22 = g17-3;");
    f.line("char *g23[] = {g17+0, g17+3, g17-3};");
    f.line("int g24=3;");
    f.line("int *g25=&g24;");
    f.line("int g26[3] = {1, 2, 3};");
    f.line("int *g27 = g26 + 1;");
    f.line("int *g28 = &g11[1].a;");
    f.line("long g29 = (long)(long)g26;");
    f.line("struct { struct { int a[3]; } a; } g30 = {{{1,2,3}}};");
    f.line("int *g31=g30.a.a;");
    f.line("struct {int a[2];} g40[2] = {{1, 2}, 3, 4};");
    f.line("struct {int a[2];} g41[2] = {1, 2, 3, 4};");
    f.line("char g43[][4] = {'f', 'o', 'o', 0, 'b', 'a', 'r', 0};");
    f.line(r#"char *g44 = {"foo"};"#);
    f.main();

    f.assert(1, "({ int x[3]={1,2,3}; x[0]; })");
    f.assert(2, "({ int x[3]={1,2,3}; x[1]; })");
    f.assert(3, "({ int x[3]={1,2,3}; x[2]; })");
    f.assert(3, "({ int x[3]={1,2,3,4}; x[2]; })");

    f.assert(2, "({ int x[2][3]={{1,2,3},{4,5,6}}; x[0][1]; })");
    f.assert(4, "({ int x[2][3]={{1,2,3},{4,5,6}}; x[1][0]; })");
    f.assert(6, "({ int x[2][3]={{1,2,3},{4,5,6}}; x[1][2]; })");

    f.assert(0, "({ int x[3]={}; x[0]; })");
    f.assert(0, "({ int x[3]={}; x[1]; })");
    f.assert(0, "({ int x[3]={}; x[2]; })");

    f.assert(2, "({ int x[2][3]={{1,2}}; x[0][1]; })");
    f.assert(0, "({ int x[2][3]={{1,2}}; x[1][0]; })");
    f.assert(0, "({ int x[2][3]={{1,2}}; x[1][2]; })");

    f.assert(b'a', r#"({ char x[4]="abc"; x[0]; })"#);
    f.assert(b'c', r#"({ char x[4]="abc"; x[2]; })"#);
    f.assert(0, r#"({ char x[4]="abc"; x[3]; })"#);
    f.assert(b'a', r#"({ char x[2][4]={"abc","def"}; x[0][0]; })"#);
    f.assert(0, r#"({ char x[2][4]={"abc","def"}; x[0][3]; })"#);
    f.assert(b'd', r#"({ char x[2][4]={"abc","def"}; x[1][0]; })"#);
    f.assert(b'f', r#"({ char x[2][4]={"abc","def"}; x[1][2]; })"#);

    f.assert(4, "({ int x[]={1,2,3,4}; x[3]; })");
    f.assert(16, "({ int x[]={1,2,3,4}; sizeof(x); })");
    f.assert(4, r#"({ char x[]="foo"; sizeof(x); })"#);

    f.assert(4, r#"({ typedef char T[]; T x="foo"; T y="x"; sizeof(x); })"#);
    f.assert(2, r#"({ typedef char T[]; T x="foo"; T y="x"; sizeof(y); })"#);
    f.assert(2, r#"({ typedef char T[]; T x="x"; T y="foo"; sizeof(x); })"#);
    f.assert(4, r#"({ typedef char T[]; T x="x"; T y="foo"; sizeof(y); })"#);

    f.assert(1, "({ struct {int a; int b; int c;} x={1,2,3}; x.a; })");
    f.assert(2, "({ struct {int a; int b; int c;} x={1,2,3}; x.b; })");
    f.assert(3, "({ struct {int a; int b; int c;} x={1,2,3}; x.c; })");
    f.assert(1, "({ struct {int a; int b; int c;} x={1}; x.a; })");
    f.assert(0, "({ struct {int a; int b; int c;} x={1}; x.b; })");
    f.assert(0, "({ struct {int a; int b; int c;} x={1}; x.c; })");

    f.assert(1, "({ struct {int a; int b;} x[2]={{1,2},{3,4}}; x[0].a; })");
    f.assert(2, "({ struct {int a; int b;} x[2]={{1,2},{3,4}}; x[0].b; })");
    f.assert(3, "({ struct {int a; int b;} x[2]={{1,2},{3,4}}; x[1].a; })");
    f.assert(4, "({ struct {int a; int b;} x[2]={{1,2},{3,4}}; x[1].b; })");

    f.assert(0, "({ struct {int a; int b;} x[2]={{1,2}}; x[1].b; })");

    f.assert(0, "({ struct {int a; int b;} x={}; x.a; })");
    f.assert(0, "({ struct {int a; int b;} x={}; x.b; })");

    f.assert(5, "({ typedef struct {int a,b,c,d,e,f;} T; T x={1,2,3,4,5,6}; T y; y=x; y.e; })");
    f.assert(2, "({ typedef struct {int a,b;} T; T x={1,2}; T y, z; z=y=x; z.b; })");

    f.assert(1, "({ typedef struct {int a,b;} T; T x={1,2}; T y=x; y.a; })");

    f.assert(4, "({ union { int a; char b[4]; } x={0x01020304}; x.b[0]; })");
    f.assert(3, "({ union { int a; char b[4]; } x={0x01020304}; x.b[1]; })");

    f.assert(0x01020304, "({ union { struct { char a,b,c,d; } e; int f; } x={{4,3,2,1}}; x.f; })");

    f.assert(3, "g3");
    f.assert(4, "g4");
    f.assert(5, "g5");
    f.assert(6, "g6");

    f.assert(0, "g9[0]");
    f.assert(1, "g9[1]");
    f.assert(2, "g9[2]");

    f.assert(1, "g11[0].a");
    f.assert(2, "g11[0].b");
    f.assert(3, "g11[1].a");
    f.assert(4, "g11[1].b");

    f.assert(1, "g12[0].a[0]");
    f.assert(2, "g12[0].a[1]");
    f.assert(0, "g12[1].a[0]");
    f.assert(0, "g12[1].a[1]");

    f.assert(4, "g13[0].b[0]");
    f.assert(3, "g13[0].b[1]");
    f.assert(8, "g13[1].b[0]");
    f.assert(7, "g13[1].b[1]");

    f.assert(7, "sizeof(g17)");
    f.assert(10," sizeof(g18)");
    f.assert(3, "sizeof(g19)");

    f.assert(0, r#"memcmp(g17, "foobar", 7)"#);
    f.assert(0, r#"memcmp(g18, "foobar\0\0\0", 10)"#);
    f.assert(0, r#"memcmp(g19, "foo", 3)"#);

    f.assert(0, r#"strcmp(g20, "foobar")"#);
    f.assert(0, r#"strcmp(g21, "bar")"#);
    f.assert(0, r#"strcmp(g22+3, "foobar")"#);

    f.assert(0, r#"strcmp(g23[0], "foobar")"#);
    f.assert(0, r#"strcmp(g23[1], "bar")"#);
    f.assert(0, r#"strcmp(g23[2]+3, "foobar")"#);

    f.assert(3, "g24");
    f.assert(3, "*g25");
    f.assert(2, "*g27");
    f.assert(3, "*g28");
    f.assert(1, "*(int *)g29");

    f.assert(1, "g31[0]");
    f.assert(2, "g31[1]");
    f.assert(3, "g31[2]");

    f.assert(1, "g40[0].a[0]");
    f.assert(2, "g40[0].a[1]");
    f.assert(3, "g40[1].a[0]");
    f.assert(4, "g40[1].a[1]");

    f.assert(1, "g41[0].a[0]");
    f.assert(2, "g41[0].a[1]");
    f.assert(3, "g41[1].a[0]");
    f.assert(4, "g41[1].a[1]");

    f.assert(0, "({ int x[2][3]={0,1,2,3,4,5}; x[0][0]; })");
    f.assert(3, "({ int x[2][3]={0,1,2,3,4,5}; x[1][0]; })");

    f.assert(0, "({ struct {int a; int b;} x[2]={0,1,2,3}; x[0].a; })");
    f.assert(2, "({ struct {int a; int b;} x[2]={0,1,2,3}; x[1].a; })");

    f.assert(0, r#"strcmp(g43[0], "foo")"#);
    f.assert(0, r#"strcmp(g43[1], "bar")"#);
    f.assert(0, r#"strcmp(g44, "foo")"#);

    f.assert(3, "({ int a[]={1,2,3,}; a[2]; })");
    f.assert(1, "({ struct {int a,b,c;} x={1,2,3,}; x.a; })");
    f.assert(1, "({ union {int a; char b;} x={1,}; x.a; })");
    f.assert(2, "({ enum {x,y,z,}; z; })");

    f.finish();
    f.run("initializer");
}

#[rustfmt::skip]
#[test]
fn test_literal() {
    let mut f = Fixture::new();
    f.main();

    f.assert(97, "'a'");
    f.assert(10, "'\\n'");
    f.assert(-128, "'\\x80'");

    f.assert(511, "0777");
    f.assert(0, "0x0");
    f.assert(10, "0xa");
    f.assert(10, "0XA");
    f.assert(48879, "0xbeef");
    f.assert(48879, "0xBEEF");
    f.assert(48879, "0XBEEF");
    f.assert(0, "0b0");
    f.assert(1, "0b1");
    f.assert(47, "0b101111");
    f.assert(47, "0B101111");

    f.assert(4, "sizeof(0)");
    f.assert(8, "sizeof(0L)");
    f.assert(8, "sizeof(0LU)");
    f.assert(8, "sizeof(0UL)");
    f.assert(8, "sizeof(0LL)");
    f.assert(8, "sizeof(0LLU)");
    f.assert(8, "sizeof(0Ull)");
    f.assert(8, "sizeof(0l)");
    f.assert(8, "sizeof(0ll)");
    f.assert(8, "sizeof(0x0L)");
    f.assert(8, "sizeof(0b0L)");
    f.assert(4, "sizeof(2147483647)");
    f.assert(8, "sizeof(2147483648)");
    f.assert(-1, "0xffffffffffffffff");
    f.assert(8, "sizeof(0xffffffffffffffff)");
    f.assert(4, "sizeof(4294967295U)");
    f.assert(8, "sizeof(4294967296U)");

    f.assert(3, "-1U>>30");
    f.assert(3, "-1Ul>>62");
    f.assert(3, "-1ull>>62");

    f.assert(1, "0xffffffffffffffffl>>63");
    f.assert(1, "0xffffffffffffffffll>>63");

    f.assert(-1, "18446744073709551615");
    f.assert(8, "sizeof(18446744073709551615)");
    f.assert(-1, "18446744073709551615>>63");

    f.assert(-1, "0xffffffffffffffff");
    f.assert(8, "sizeof(0xffffffffffffffff)");
    f.assert(1, "0xffffffffffffffff>>63");

    f.assert(-1, "01777777777777777777777");
    f.assert(8, "sizeof(01777777777777777777777)");
    f.assert(1, "01777777777777777777777>>63");

    f.assert(-1, "0b1111111111111111111111111111111111111111111111111111111111111111");
    f.assert(8, "sizeof(0b1111111111111111111111111111111111111111111111111111111111111111)");
    f.assert(1, "0b1111111111111111111111111111111111111111111111111111111111111111>>63");

    f.assert(8, "sizeof(2147483648)");
    f.assert(4, "sizeof(2147483647)");

    f.assert(8, "sizeof(0x1ffffffff)");
    f.assert(4, "sizeof(0xffffffff)");
    f.assert(1, "0xffffffff>>31");

    f.assert(8, "sizeof(040000000000)");
    f.assert(4, "sizeof(037777777777)");
    f.assert(1, "037777777777>>31");

    f.assert(8, "sizeof(0b111111111111111111111111111111111)");
    f.assert(4, "sizeof(0b11111111111111111111111111111111)");
    f.assert(1, "0b11111111111111111111111111111111>>31");

    f.assert(-1, "1 << 31 >> 31");
    f.assert(-1, "01 << 31 >> 31");
    f.assert(-1, "0x1 << 31 >> 31");
    f.assert(-1, "0b1 << 31 >> 31");

    f.line("0.0;");
    f.line("1.0;");
    f.line("3e+8;");
    f.line("0x10.1p0;");
    f.line(".1E4f;");

    f.assert(4, "sizeof(8.f)");
    f.assert(4, "sizeof(0.3F)");
    f.assert(8, "sizeof(0.)");
    f.assert(8, "sizeof(.0)");
    f.assert(8, "sizeof(5.l)");
    f.assert(8, "sizeof(2.0L)");

    f.assert(1, "size\\\nof(char)");

    f.finish();
    f.run("literal");
}

#[rustfmt::skip]
#[test]
fn test_macro() {
    let mut f = Fixture::new();
    f.includes.insert("include1.h", "#include \"include2.h\"\nchar *include1_filename = __FILE__;\nint include1_line = __LINE__;\nint include1 = 5;");
    f.includes.insert("include2.h", "int include2 = 7;");
    f.includes.insert("include3.h", "#define foo 3");
    f.includes.insert("include4.h", "#define foo 4");

    f.line("char *main_filename1 = __FILE__;");
    f.line("int main_line1 = __LINE__;");
    f.line("#define LINE() __LINE__");
    f.line("int main_line2 = LINE();");

    f.line("#include \"include1.h\"");
    f.line("#");
    f.line("/* */ #");
    f.line("int ret3(void) { return 3; }");
    f.line("int dbl(int x) { return x*x; }");
    f.line("int add2(int x, int y) { return x+y; }");
    f.line("int add6(int a, int b, int c, int d, int e, int f) { return a+b+c+d+e+f; }");
    f.main();

    f.assert(5, "include1");
    f.assert(7, "include2");

    f.line("#include \"include3.h\"");
    f.assert(3, "foo");
    f.line("#include \"include4.h\"");
    f.assert(4, "foo");

    f.line("#define INCLUDE3 \"include3.h\"");
    f.line("#include INCLUDE3");
    f.assert(3, "foo");

    f.line("#define INCLUDE4 < include4.h");
    f.line("#include INCLUDE4 >");
    f.assert(4, "foo");

    f.line("#undef foo");

    f.line("#if 0");
    f.line("#include \"/no/such/file\"");
    f.line("#invalid directive");
    f.assert(0, "1");
    f.line("#if nested");
    f.line("#endif // nested");
    f.line("#endif");

    f.line("int m = 0;");
    f.line("#if 1");
    f.line("m = 5;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("#if 1 // +1");
    f.line("#if 0 // +2");
    f.line("#if 1 // +3");
    f.line("foo bar");
    f.line("#endif // -3");
    f.line("#endif // -2");
    f.line("m = 3;");
    f.line("#endif // -1");
    f.assert(3, "m");

    f.line("#if 1-1 // +1");
    f.line("#if 1 // +2");
    f.line("#endif // -2");
    f.line("#if 1 // +3");
    f.line("#else");
    f.line("#endif // -3");
    f.line("#if 0 // +4");
    f.line("#else");
    f.line("#endif // -4");
    f.line("m = 2;");
    f.line("#else");
    f.line("#if 1 // +5");
    f.line("m = 3;");
    f.line("#endif // -5");
    f.line("#endif // -1");
    f.assert(3, "m");

    f.line("#if 1");
    f.line("m = 2;");
    f.line("#else");
    f.line("m = 3;");
    f.line("#endif");
    f.assert(2, "m");

    f.line("#if 1");
    f.line("m = 2;");
    f.line("#else");
    f.line("m = 3;");
    f.line("#endif");
    f.assert(2, "m");

    f.line("#if 0");
    f.line("m = 1;");
    f.line("#elif 0");
    f.line("m = 2;");
    f.line("#elif 3+5");
    f.line("m = 3;");
    f.line("#elif 1*5");
    f.line("m = 4;");
    f.line("#endif");
    f.assert(3, "m");

    f.line("#if 1+5");
    f.line("m = 1;");
    f.line("#elif 1");
    f.line("m = 2;");
    f.line("#elif 3");
    f.line("m = 2;");
    f.line("#endif");
    f.assert(1, "m");

    f.line("#if 0 // +1");
    f.line("m = 1;");
    f.line("#elif 1");
    f.line("#if 1 // +2");
    f.line("m = 2;");
    f.line("#else");
    f.line("m = 3;");
    f.line("#endif // -2");
    f.line("#else");
    f.line("m = 5;");
    f.line("#endif // -1");
    f.assert(2, "m");

    f.line("int M1 = 5;");
    f.line("#define M1 3");
    f.assert(3, "M1");
    f.line("#define M1 4");
    f.assert(4, "M1");

    f.line("#define M2 3+4+");
    f.assert(12, "M2 5");

    f.line("#define M3 3+4");
    f.assert(23, "M3*5");

    f.line("#define ASSERT_ assert(");
    f.line("#define if 5");
    f.line("#define five \"5\"");
    f.line("#define END )");
    f.line("ASSERT_ 5, if, five END;");

    f.line("#undef ASSERT_");
    f.line("#undef if");
    f.line("#undef five");
    f.line("#undef END");
    f.line("if (0);");

    f.line("#define M4 5");
    f.line("#if M4");
    f.line("m = 5;");
    f.line("#else");
    f.line("m = 6;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("#if M4-5");
    f.line("m = 6;");
    f.line("#elif M4");
    f.line("m = 5;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("int M5 = 6;");
    f.line("#define M5 M5 + 3");
    f.assert(9, "M5");

    f.line("#define M6 M5 + 3");
    f.assert(12, "M6");

    f.line("int M7 = 3;");
    f.line("#define M7 M8 * 5");
    f.line("#define M8 M7 + 2");
    f.assert(13, "M7");

    f.line("#ifdef M9");
    f.line("m = 5;");
    f.line("#else");
    f.line("m = 3;");
    f.line("#endif");
    f.assert(3, "m");

    f.line("#define M9");
    f.line("#ifdef M9");
    f.line("m = 5;");
    f.line("#else");
    f.line("m = 3;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("#ifndef M10");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 5;");
    f.line("#endif");
    f.assert(3, "m");

    f.line("#define M10");
    f.line("#ifndef M10");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 5;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("#if 0");
    f.line("#ifdef NO_SUCH_MACRO");
    f.line("#endif");
    f.line("#ifndef NO_SUCH_MACRO");
    f.line("#endif");
    f.line("#else");
    f.line("#endif");

    f.line("#define M11() 1");
    f.line("int M11 = 5;");
    f.assert(1, "M11()");
    f.assert(5, "M11");

    f.line("#define M11 ()");
    f.assert(3, "ret3 M11");

    f.line("#define M12(x,y) x+y");
    f.assert(7, "M12(3, 4)");

    f.line("#define M12(x,y) x*y");
    f.assert(24, "M12(3+4, 4+5)");

    f.line("#define M12(x,y) (x)*(y)");
    f.assert(63, "M12(3+4, 4+5)");

    f.line("#define M12(x,y) x y");
    f.assert(9, "M12(, 4+5)");

    f.line("#define M12(x,y) x*y");
    f.assert(20, "M12((2+3), 4)");

    f.line("#define M12(x,y) x*y");
    f.assert(12, "M12((2,3), 4)");

    f.line("#define dbl(x) M13(x) * x");
    f.line("#define M13(x) dbl(x) + 3");
    f.assert(10, "dbl(2)");

    f.line("#define M14(x) #x");
    f.assert("'a'", "M14( a!b  `\"\"c)[0]");
    f.assert("'!'", "M14( a!b  `\"\"c)[1]");
    f.assert("'b'", "M14( a!b  `\"\"c)[2]");
    f.assert("' '", "M14( a!b  `\"\"c)[3]");
    f.assert("'`'", "M14( a!b  `\"\"c)[4]");
    f.assert("'\"'", "M14( a!b  `\"\"c)[5]");
    f.assert("'\"'", "M14( a!b  `\"\"c)[6]");
    f.assert("'c'", "M14( a!b  `\"\"c)[7]");
    f.assert(0, "M14( a!b  `\"\"c)[8]");

    f.line("#define paste(x,y) x##y");
    f.assert(15, "paste(1,5)");
    f.assert(255, "paste(0,xff)");
    f.assert(3, "({ int foobar=3; paste(foo,bar); })");
    f.assert(5, "paste(5,)");
    f.assert(5, "paste(,5)");

    f.line("#define i 5");
    f.assert(101, "({ int i3=100; paste(1+i,3); })");
    f.line("#undef i");

    f.line("#define paste2(x) x##5");
    f.assert(26, "paste2(1+2)");

    f.line("#define paste3(x) 2##x");
    f.assert(23, "paste3(1+2)");

    f.line("#define paste4(x, y, z) x##y##z");
    f.assert(123, "paste4(1,2,3)");

    f.line("#define paste5 +##+");
    f.assert(4, "({ int x=3; paste5 x; })");

    f.line("#define paste6 foo##bar");
    f.assert(3, "({ int foobar=3; paste6; })");

    f.line("#define paste7 0##xff");
    f.assert(255, "paste7");

    f.line("#define paste8 1##2##3");
    f.assert(123, "paste8");

    f.line("#define M15");
    f.line("#if defined(M15)");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 4;");
    f.line("#endif");
    f.assert(3, "m");

    f.line("#if defined M15");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 4;");
    f.line("#endif");
    f.assert(3, "m");

    f.line("#if defined(M15) - 1");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 4;");
    f.line("#endif");
    f.assert(4, "m");

    f.line("#undef M15");
    f.line("#if defined M15");
    f.line("m = 3;");
    f.line("#else");
    f.line("m = 4;");
    f.line("#endif");
    f.assert(4, "m");

    f.line("#if NO_SUCH_SYMBOL == 0");
    f.line("m = 5;");
    f.line("#else");
    f.line("m = 6;");
    f.line("#endif");
    f.assert(5, "m");

    f.line("#define STR(x) #x");
    f.line("#define M16(x) STR(x)");
    f.line("#define M17(x) M16(foo.x)");
    f.assert(0, "strcmp(M17(bar), \"foo.bar\")");
    f.line("#define M18(x) M16(foo. x)");
    f.assert(0, "strcmp(M18(bar), \"foo. bar\")");

    f.line("#define M19 foo");
    f.line("#define M20(x) STR(x)");
    f.line("#define M21(x) M20(x.M19)");
    f.assert(0, "strcmp(M21(bar), \"bar.foo\")");
    f.line("#define M22(x) M20(x. M19)");
    f.assert(0, "strcmp(M22(bar), \"bar. foo\")");

    f.assert(1, "__chacc__");

    f.assert(0, &format!("strcmp(main_filename1, \"{}\")", f.tmp.path().join("macro.c").display()));
    f.assert(6, "main_line1");
    f.assert(8, "main_line2");
    f.assert(0, &format!("strcmp(include1_filename, \"{}\")", f.tmp.path().join("include1.h").display()));
    f.assert(3, "include1_line");

    f.line("#define M23(...) 3");
    f.assert(3, "M23()");
    f.line("#define M23(x, ...) x");
    f.assert(5, "M23(5)");

    f.line("#define M24(...) __VA_ARGS__");
    f.assert(2, "M24() 2");
    f.assert(5, "M24(5)");

    f.line("#define M25(...) add2(__VA_ARGS__)");
    f.assert(8, "M25(2,6)");

    f.line("#define M26(...) add6(1,2,__VA_ARGS__,6)");
    f.assert(21, "M26(3,4,5)");
    f.line("#define M26(x, ...) add6(1,2,x,__VA_ARGS__,6)");
    f.assert(21, "M26(3,4,5)");

    f.finish();
    f.run("macro");
}

#[rustfmt::skip]
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

#[rustfmt::skip]
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

    f.assert(8, "sizeof(int(*)[10])");
    f.assert(8, "sizeof(int(*)[][10])");

    f.assert(4, "sizeof(struct { int x, y[]; })");

    f.assert(1, "sizeof(char)");
    f.assert(1, "sizeof(signed char)");
    f.assert(1, "sizeof(unsigned char)");

    f.assert(2, "sizeof(short)");
    f.assert(2, "sizeof(int short)");
    f.assert(2, "sizeof(short int)");
    f.assert(2, "sizeof(signed short)");
    f.assert(2, "sizeof(int short signed)");
    f.assert(2, "sizeof(unsigned short)");
    f.assert(2, "sizeof(int short unsigned)");

    f.assert(4, "sizeof(int)");
    f.assert(4, "sizeof(signed int)");
    f.assert(4, "sizeof(signed)");
    f.assert(4, "sizeof(unsigned int)");
    f.assert(4, "sizeof(unsigned)");

    f.assert(8, "sizeof(long)");
    f.assert(8, "sizeof(signed long)");
    f.assert(8, "sizeof(signed long int)");
    f.assert(8, "sizeof(unsigned long)");
    f.assert(8, "sizeof(unsigned long int)");

    f.assert(8, "sizeof(long long)");
    f.assert(8, "sizeof(signed long long)");
    f.assert(8, "sizeof(signed long long int)");
    f.assert(8, "sizeof(unsigned long long)");
    f.assert(8, "sizeof(unsigned long long int)");

    f.assert(1, "sizeof((char)1)");
    f.assert(2, "sizeof((short)1)");
    f.assert(4, "sizeof((int)1)");
    f.assert(8, "sizeof((long)1)");

    f.assert(4, "sizeof((char)1 + (char)1)");
    f.assert(4, "sizeof((short)1 + (short)1)");
    f.assert(4, "sizeof(1?2:3)");
    f.assert(4, "sizeof(1?(short)2:(char)3)");
    f.assert(8, "sizeof(1?(long)2:(char)3)");

    f.assert(1, "sizeof(char) << 31 >> 31");
    f.assert(1, "sizeof(char) << 63 >> 63");

    f.assert(4, "sizeof(float)");
    f.assert(8, "sizeof(double)");
    f.assert(8, "sizeof(long double)");

    f.assert(4, "sizeof(1.f+2)");
    f.assert(8, "sizeof(1.0+2)");
    f.assert(4, "sizeof(1.f-2)");
    f.assert(8, "sizeof(1.0-2)");
    f.assert(4, "sizeof(1.f*2)");
    f.assert(8, "sizeof(1.0*2)");
    f.assert(4, "sizeof(1.f/2)");
    f.assert(8, "sizeof(1.0/2)");

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

    f.assert(7, r#"sizeof("abc" "def")"#);
    f.assert(9, r#"sizeof("abc" "d" "efgh")"#);
    f.assert(0, r#"strcmp("abc" "d" "\nefgh", "abcd\nefgh")"#);
    f.assert(0, r#"!strcmp("abc" "d", "abcd\nefgh")"#);
    f.assert(0, r#"strcmp("\x9" "0", "\t0")"#);

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

    f.assert(8, "({ struct foo *bar; sizeof(bar); })");
    f.assert(4, "({ struct T *foo; struct T {int x;}; sizeof(struct T); })");
    f.assert(1, "({ struct T { struct T *next; int x; } a; struct T b; b.x=1; a.next=&b; a.next->x; })");
    f.assert(4, "({ typedef struct T T; struct T { int x; }; sizeof(T); })");

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
fn test_usual_conv() {
    let mut f = Fixture::new();
    f.line("static int ret10(void) { return 10; }");
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

    f.assert(10, "(1 ? ret10 : (void *)0)()");

    f.finish();
    f.run("usual_conv");
}

#[rustfmt::skip]
#[test]
fn test_variable() {
    let mut f = Fixture::new();
    f.line("int g1, g2[4];");
    f.line("static int g3 = 3;");
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

    f.assert(3, "g3");

    f.finish();
    f.run("variable");
}

#[test]
fn test_help_flag() {
    let output = Command::chacc(".")
        .arg("--help")
        .run_checked("running with --help flag", None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("chacc"));
}

#[test]
fn test_output_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");

    let input = create_c(tmp.path()).expect("failed to create input");
    let exe = tmp.path().join("app.exe");
    Command::chacc(tmp.path())
        .arg("-o")
        .arg(&exe)
        .arg(&input)
        .run_checked("compiling with -o flag", None);
    assert!(exe.is_file());
    Command::new(&exe).run_checked("running output executable", Some(42));

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let exe = tmp.path().join("app-multi.exe");
    let mut command = Command::chacc(tmp.path());
    command.arg("-o").arg(&exe);
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling multiple inputs with -o flag", None);
    assert!(exe.is_file());
    Command::new(&exe).run_checked("running output executable", Some(42));
}

#[test]
fn test_include_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");

    let dir1 = tmp.path().join("dir1");
    let dir2 = tmp.path().join("dir2");
    std::fs::create_dir(&dir1).expect("failed to create subdirectory");
    std::fs::create_dir(&dir2).expect("failed to create subdirectory");
    std::fs::write(dir1.join("foo.h"), "foo").expect("failed to write file");
    std::fs::write(dir2.join("bar.h"), "bar").expect("failed to write file");

    let input = tmp.path().join("main.c");
    std::fs::write(&input, "#include \"foo.h\"\n#include \"bar.h\"\n")
        .expect("failed to write input");

    let output = Command::chacc(tmp.path())
        .arg("-I")
        .arg(&dir1)
        .arg("-I./dir2")
        .arg("-E")
        .arg("-o")
        .arg("-")
        .arg(&input)
        .run_checked("compiling with -I flag", None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("foo"));
    assert!(stdout.contains("bar"));
}

#[test]
fn test_preprocess_only_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");

    let input = create_c(tmp.path()).expect("failed to create input");
    let output = Command::chacc(tmp.path())
        .arg("-E")
        .arg("-o")
        .arg("-")
        .arg(&input)
        .run_checked("compiling with -E", None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let content = std::fs::read_to_string(&input).expect("failed to read input");
    assert!(stdout.contains(&content));

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let mut command = Command::chacc(tmp.path());
    command.arg("-E");
    for input in &inputs {
        command.arg(input);
    }
    let output = command.run_checked("compiling multiple inputs with -E", None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for input in &inputs {
        let content = std::fs::read_to_string(input).expect("failed to read input");
        assert!(stdout.contains(&content));
    }

    let mut command = Command::chacc(tmp.path());
    command.arg("-E").arg("-o").arg("-");
    for input in &inputs {
        command.arg(input);
    }
    let output = command.run_checked("compiling multiple inputs with -S and -o", Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot specify '-o'"));
    assert!(stderr.contains("'-E'"));
    assert!(stderr.contains("with multiple files"));
}

#[test]
fn test_assemble_only_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");

    let input = create_c(tmp.path()).expect("failed to create input");
    let output = Command::chacc(tmp.path())
        .arg("-S")
        .arg("-o")
        .arg("-")
        .arg(&input)
        .run_checked("compiling with -S", None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main:"));

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let mut command = Command::chacc(tmp.path());
    command.arg("-S");
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling multiple inputs with -S", None);
    for input in &inputs {
        assert!(input.with_extension("s").exists());
    }

    let mut command = Command::chacc(tmp.path());
    command.arg("-S").arg("-o").arg("-");
    for input in &inputs {
        command.arg(input);
    }
    let output = command.run_checked("compiling multiple inputs with -S and -o", Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot specify '-o'"));
    assert!(stderr.contains("'-S'"));
    assert!(stderr.contains("with multiple files"));
}

#[test]
fn test_compile_only_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");

    let input = create_c(tmp.path()).expect("failed to create input");
    let obj = input.with_extension("o");
    Command::chacc(tmp.path())
        .arg("-c")
        .arg("-o")
        .arg(&obj)
        .arg(&input)
        .run_checked("compiling with -c", None);
    assert!(obj.exists());

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let mut command = Command::chacc(tmp.path());
    command.arg("-c");
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling multiple inputs with -c", None);
    for input in &inputs {
        assert!(input.with_extension("o").exists());
    }

    let mut command = Command::chacc(tmp.path());
    command.arg("-c").arg("-o").arg(&obj);
    for input in &inputs {
        command.arg(input);
    }
    let output = command.run_checked("compiling multiple inputs with -c and -o", Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot specify '-o'"));
    assert!(stderr.contains("'-c'"));
    assert!(stderr.contains("with multiple files"));
}

#[test]
fn test_save_temps_flag() {
    let tmp = tempdir().expect("failed to create temporary directory");
    let output_dir = tmp.path().join("output");
    std::fs::create_dir(&output_dir).expect("failed to create output directory");

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let mut command = Command::chacc(tmp.path());
    command.arg("-save-temps");
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling with -save-temps", None);
    assert!(tmp.path().join("a.out").is_file());
    for input in &inputs {
        let mut file_name = OsString::from("a-");
        file_name.push(input.file_stem().unwrap());
        let path = tmp.path().join(file_name);
        assert!(path.with_extension("o").is_file());
        assert!(path.with_extension("s").is_file());
    }

    let exe = output_dir.join("app.exe");
    let mut command = Command::chacc(tmp.path());
    command.arg("-save-temps").arg("-o").arg(&exe);
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling with -save-temps and -o", None);
    assert!(exe.is_file());
    for input in &inputs {
        let mut file_name = OsString::from("app-");
        file_name.push(input.file_stem().unwrap());
        let path = output_dir.join(file_name);
        assert!(path.with_extension("o").is_file());
        assert!(path.with_extension("s").is_file());
    }

    let tmp = tempdir().expect("failed to create temporary directory");

    let inputs = create_multi_c(tmp.path()).expect("failed to create inputs");
    let mut command = Command::chacc(tmp.path());
    command.arg("-save-temps").arg("-c");
    for input in &inputs {
        command.arg(input);
    }
    command.run_checked("compiling with -save-temps and -c", None);
    for input in &inputs {
        assert!(input.with_extension("o").is_file());
        assert!(input.with_extension("s").is_file());
    }
}
