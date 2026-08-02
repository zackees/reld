ENTRY(begin_here)

SECTIONS
{
    .text : {
        . = 0x10;
        *(.text)
        . = 0x11;
    }
}
