//! Memory management operations for INT 21h
//!
//! Memory allocation, free, and resize operations (AH=48h, 49h, 4Ah, 58h)
//! are currently handled inline in the INT 21h dispatch table.
//! Helper methods will be extracted here as the codebase evolves.
