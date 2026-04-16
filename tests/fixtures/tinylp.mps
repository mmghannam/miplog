NAME          tinylp
* Small LP with 3 continuous vars and 2 constraints. Bounded, feasible,
* unique optimum. Used to validate LP-only parser code paths (no MIP
* progress table, no cuts, no incumbents — just LP iterations + final).
*
* min  x1 + 3 x2 - x3
* s.t. 2 x1 + 3 x2 + 4 x3  <=  20
*       x1 + 2 x2 +   x3  >=   2
*      x1, x2, x3 >= 0
ROWS
 N  obj
 L  c1
 G  c2
COLUMNS
    x1        obj            1.0   c1             2.0
    x1        c2             1.0
    x2        obj            3.0   c1             3.0
    x2        c2             2.0
    x3        obj           -1.0   c1             4.0
    x3        c2             1.0
RHS
    rhs       c1            20.0
    rhs       c2             2.0
BOUNDS
ENDATA
