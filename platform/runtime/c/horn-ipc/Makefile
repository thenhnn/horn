CC ?= cc
AR ?= ar
CFLAGS ?= -O2 -Wall -Wextra -Iinclude

all: libhorn_ipc.a

libhorn_ipc.a: src/horn_ipc.o
	$(AR) rcs $@ $<

src/horn_ipc.o: src/horn_ipc.c include/horn_ipc.h
	$(CC) $(CFLAGS) -c -o $@ $<

sel4: libhorn_ipc_sel4.a

libhorn_ipc_sel4.a: src/horn_ipc_sel4.o
	$(AR) rcs $@ $<

src/horn_ipc_sel4.o: src/horn_ipc_sel4.c include/horn_ipc_sel4.h include/horn_ipc.h
	$(CC) $(CFLAGS) -I$(SEL4_INCLUDE) -c -o $@ $<

test: libhorn_ipc.a
	$(CC) $(CFLAGS) tests/layout.c libhorn_ipc.a -o tests/layout-test
	./tests/layout-test
	rm -f tests/layout-test

clean:
	rm -f src/horn_ipc.o src/horn_ipc_sel4.o libhorn_ipc.a libhorn_ipc_sel4.a tests/layout-test
