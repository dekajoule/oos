Changed:

- The short time bases (fewer samples than screen columns) interpolate their points in fixed point, as fumingcaps'
  fork does: about half the cost of a frame's points on the scope's small cores, more waveforms a second at 5 to
  200 ns/div. The trace stays within a pixel of the one before.
