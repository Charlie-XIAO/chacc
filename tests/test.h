#ifndef TEST_H_
#define TEST_H_

#define ASSERT(expected, actual) assert((expected), (actual), #actual)

int assert(int expected, int actual, char *code);
int strcmp(char *lhs, char *rhs);
int memcmp(char *lhs, char *rhs, int n);
void exit(int code);

#endif // TEST_H_
