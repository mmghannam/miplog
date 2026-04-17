NAME          unbounded
* Trivially unbounded LP: minimize -x with x free. No finite minimum.
* Every modern solver detects unboundedness in presolve or at the root LP.
ROWS
 N  obj
COLUMNS
    x         obj           -1.0
RHS
BOUNDS
 FR bnd       x
ENDATA
