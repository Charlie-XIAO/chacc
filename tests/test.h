#ifndef TEST_H_
#define TEST_H_

#define ASSERT(expected, actual) assert((expected), (actual), #actual)

int assert(int expected, int actual, char *code);

#endif // TEST_H_
