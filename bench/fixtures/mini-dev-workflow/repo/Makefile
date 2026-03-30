CC ?= gcc
CFLAGS ?= -O2

all: src/hello

src/hello.o: src/hello.c src/hello.h
	$(CC) $(CFLAGS) -c src/hello.c -o src/hello.o

src/hello: src/hello.o
	$(CC) src/hello.o -o src/hello

clean:
	rm -f src/hello.o src/hello
