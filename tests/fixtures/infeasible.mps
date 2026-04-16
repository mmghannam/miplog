NAME          infeasible
* Trivially infeasible MIP: x is binary, must be >= 1 and <= 0.
* Every modern solver detects this in presolve.
ROWS
 N  obj
 G  c1
 L  c2
COLUMNS
    MARKER                 'MARKER'                 'INTORG'
    x         obj            1.0
    x         c1             1.0
    x         c2             1.0
    MARKER                 'MARKER'                 'INTEND'
RHS
    rhs       c1             1.0
    rhs       c2             0.0
BOUNDS
 BV bnd       x
ENDATA
