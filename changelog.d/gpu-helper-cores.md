Changed:

- The GPU helper may run on every core instead of only the two fast ones it shared with the screen loop and the
  acquisition: in Average with 4 channels the screen loop no longer waits for it, about 10 % more waveforms a second.
