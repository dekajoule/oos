Fixed:

- MHO900 test build: a generator setting the board did not take (the window not mapped, the relays not set) is tried
  again after 5 s instead of never, and the generators are switched off when the acquisition ends
  (`oos-mho-live off`) ([MHO900](docs/mho.md)).
