NASM ?= nasm
all: caller.com answer.com caller-ath.com answer-auto.com
caller.com: sertest.asm
	$(NASM) -f bin -DBASE=0x2f8 -DCALLER=1 -DHANG_DTR=1 -o $@ $<
answer.com: sertest.asm
	$(NASM) -f bin -DBASE=0x2f8 -DCALLER=0 -DAUTO=0 -o $@ $<
caller-ath.com: sertest.asm
	$(NASM) -f bin -DBASE=0x3f8 -DCALLER=1 -DHANG_DTR=0 -o $@ $<
answer-auto.com: sertest.asm
	$(NASM) -f bin -DBASE=0x3f8 -DCALLER=0 -DAUTO=1 -o $@ $<
