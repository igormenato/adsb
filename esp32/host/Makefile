CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame

test_trck_frame: test_trck_frame.c
	$(CC) $(CFLAGS) -o $@ test_trck_frame.c

clean:
	rm -f test_trck_frame
