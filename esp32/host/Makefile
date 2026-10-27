CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -I../trck_uart
SRC = test_trck_frame.c ../trck_uart/trck_frame.c

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame

test_trck_frame: $(SRC) ../trck_uart/trck_frame.h
	$(CC) $(CFLAGS) -o $@ $(SRC)

clean:
	rm -f test_trck_frame
