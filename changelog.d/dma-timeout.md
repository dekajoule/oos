Fixed:

- A record transfer the FPGA does not finish stops the acquisition for 1 s instead of 10 s; the scope then sets the
  acquisition up again by itself, waiting longer when it happens again and again. `:SYSTem:STATe?` counts them
  (`dma timeouts`).
