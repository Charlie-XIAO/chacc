#include <stdio.h>
#include <stdlib.h>

int assert(int expected, int actual, char *code) {
  if (expected == actual) {
    printf("%s => \x1b[32m%d\x1b[0m\n", code, actual);
    return actual;
  } else {
    printf("%s => \x1b[31m%d expected but got %d\x1b[0m\n", code, expected, actual);
    exit(1);
  }
}
