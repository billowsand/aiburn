CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra
LDFLAGS ?=

TARGET  = is903burn
SRC     = is903burn.c

.PHONY: all clean install

all: $(TARGET)

$(TARGET): $(SRC)
	$(CC) $(CFLAGS) -o $@ $(SRC) $(LDFLAGS)

clean:
	rm -f $(TARGET)

install: $(TARGET)
	install -Dm755 $(TARGET) $(DESTDIR)/usr/local/bin/$(TARGET)
