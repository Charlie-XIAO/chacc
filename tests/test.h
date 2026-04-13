#ifndef TEST_H_
#define TEST_H_

#define ASSERT(expected, actual) assert((expected), (actual), #actual)

void assert(int expected, int actual, char *code);
int strcmp(char *lhs, char *rhs);
int memcmp(char *lhs, char *rhs, int n);

#endif // TEST_H_
