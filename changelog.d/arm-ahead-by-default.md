Changed:

- The next record is armed while the current one is still being prepared for the screen, so more records reach it:
  two channels at 5 ms/div 12 instead of 11 a second, 2 ms/div 23 instead of 19.5, 1 ms/div 44 instead of 34
  (unit). `acq.prearm = off` in system.conf is the way back.
