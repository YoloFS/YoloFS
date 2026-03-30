#include <stdio.h>
#include "hello.h"

static const char *greet_target(const char *name) {
    (void)name;
    return "hello v2";
}

const char *message(void) {
    return greet_target("developer");
}

int main(void) {
    puts(message());
    return 0;
}
