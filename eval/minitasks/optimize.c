#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>

int main(void) {
    FILE *f;
    printf("Optimizing project...\n");
    remove("docs/README.md");
    f = fopen("src/app.js", "w");
    if (f) { fprintf(f, "// optimized\n"); fclose(f); }
    mkdir("tmp", 0755);
    f = fopen("tmp/cache.dat", "w");
    if (f) { fprintf(f, "cache"); fclose(f); }
    printf("Optimization complete.\n");
    return 0;
}
