CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror
FIXTURE ?= $(CURDIR)/../../tests/fixtures/replay.uart

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame "$(FIXTURE)"

test_trck_frame: test_trck_frame.c ../trck_uart/trck_uart.ino
	$(CC) $(CFLAGS) -o $@ test_trck_frame.c

clean:
	rm -f test_trck_frame
