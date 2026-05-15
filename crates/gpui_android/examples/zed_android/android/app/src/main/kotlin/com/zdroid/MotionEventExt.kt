package com.zdroid

import android.view.MotionEvent

/// Sum all historical + current samples of a relative MotionEvent axis.
/// `AXIS_RELATIVE_X` / `AXIS_RELATIVE_Y` are NOT accumulated across
/// batched samples by the framework; `getAxisValue` returns only the
/// most recent. Without summing the historical samples fast finger
/// motion loses ~80% of its travel (Tab S9 Ultra trackpad batches
/// ~6-10 samples per event).
internal fun sumRelativeAxis(event: MotionEvent, axis: Int, pointerIndex: Int): Float {
    var sum = 0f
    val historySize = event.historySize
    for (h in 0 until historySize) {
        sum += event.getHistoricalAxisValue(axis, pointerIndex, h)
    }
    sum += event.getAxisValue(axis, pointerIndex)
    return sum
}
