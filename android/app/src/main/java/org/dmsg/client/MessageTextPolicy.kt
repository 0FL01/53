package org.dmsg.client

/** Raw TEXT/EDIT source is limited by Unicode scalar values, not UTF-16 units or bytes. */
internal object MessageTextPolicy {
    const val MAX_SCALARS = 4000

    /** A lone surrogate has no scalar value; reject it before UTF-8 encoding can replace it. */
    fun count(text: CharSequence): Int? {
        var index = 0
        var count = 0
        while (index < text.length) {
            val char = text[index++]
            when {
                Character.isHighSurrogate(char) -> {
                    if (index == text.length || !Character.isLowSurrogate(text[index])) return null
                    index++
                }
                Character.isLowSurrogate(char) -> return null
            }
            count++
        }
        return count
    }

    fun isValid(text: CharSequence): Boolean = count(text)?.let { it in 1..MAX_SCALARS } ?: false
}
