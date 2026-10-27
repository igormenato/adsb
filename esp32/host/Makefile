CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -I../main
SRC = test_trck_frame.c ../main/trck_frame.c

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame

test_trck_frame: $(SRC) ../main/trck_frame.h
	$(CC) $(CFLAGS) -o $@ $(SRC)

clean:
	rm -f test_trck_frame
