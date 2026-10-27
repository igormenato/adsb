CXX ?= c++
CXXFLAGS ?= -std=c++17 -Wall -Wextra -Werror
SKETCH = ../trck_uart/trck_uart.ino

.PHONY: test clean

test: test_trck_frame
	./test_trck_frame

test_trck_frame: $(SKETCH)
	$(CXX) $(CXXFLAGS) -x c++ -o $@ $(SKETCH)

clean:
	rm -f test_trck_frame
