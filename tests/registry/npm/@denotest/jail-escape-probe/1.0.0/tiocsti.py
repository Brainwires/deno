# Types into the controlling terminal (TIOCSTI), the bubblewrap
# CVE-2017-5226 class of escape; and reads the installer's environment
# through the process table (macOS: sysctl KERN_PROCARGS2).
import ctypes, fcntl, os, sys, termios

try:
    fd = os.open("/dev/tty", os.O_RDWR)
    try:
        fcntl.ioctl(fd, termios.TIOCSTI, b"#")
        print("tiocsti: allowed")
    except OSError:
        print("tiocsti: denied")
except OSError:
    print("tiocsti: denied")

# argv[1] is the jailed process (the shell's parent), its own session
# leader since DENEXT.md item 124
jailed = int(sys.argv[1])
print("own session: %s" % ("yes" if os.getsid(0) == jailed else "no"))

if sys.platform == "darwin":
    libc = ctypes.CDLL(None, use_errno=True)
    # the process table (KERN_PROC_ALL): finding the installer's pid is
    # the first step to reading its environment (KERN_PROCARGS2 holds any
    # same-user process's arguments and environment)
    mib = (ctypes.c_int * 3)(1, 14, 0)  # CTL_KERN, KERN_PROC, KERN_PROC_ALL
    size = ctypes.c_size_t(0)
    listed = libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) == 0 and size.value > 0
    print("process table: %s" % ("readable" if listed else "hidden"))
