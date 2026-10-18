# La parte en Rust busca estas funciones por nombre (Java_coroom_android_Native_*): ni la clase
# ni sus métodos nativos se pueden renombrar.
-keep class coroom.android.Native { *; }
