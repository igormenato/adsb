CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror
CPPFLAGS ?= -I../trck_uart
FIXTURE ?= $(CURDIR)/../../tests/fixtures/replay.uart

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame "$(FIXTURE)"

test_trck_frame: test_trck_frame.c ../trck_uart/trck_frame.h
	$(CC) $(CPPFLAGS) $(CFLAGS) -o $@ test_trck_frame.c

clean:
	rm -f test_trck_frame
