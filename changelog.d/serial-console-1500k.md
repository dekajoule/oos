Changed:

- The start shows the first text on the screen about 1.6 s sooner: the kernel's serial console on UART2 runs at
  1500000 baud instead of 115200, readable from the first line on. A terminal on that port switches from 115200 to
  1500000 after the loader's "Starting kernel" ([Troubleshooting](wiki/Troubleshooting.md)) (unit).
