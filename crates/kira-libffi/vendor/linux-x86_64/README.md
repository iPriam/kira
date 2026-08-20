libffi 3.5.2 Linux x86_64 runtime, the same upstream version as the Windows
artifact beside it, taken from the distribution's own build
(`libffi-3.5.2-2.fc44.x86_64`, `/usr/lib64/libffi.so.8.2.0`).

Vendored rather than resolved from the system at run time for the reason the
loader states: Kira loads its bundled libffi from the executable's or the
package's directory and consults no library search path, so a Kira program runs
against the libffi it was built with and not whichever one a host happens to
carry.
