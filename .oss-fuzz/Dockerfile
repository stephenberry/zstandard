FROM gcr.io/oss-fuzz-base/base-builder-rust

RUN git clone --depth 1 https://github.com/stephenberry/zstandard.git $SRC/zstandard

WORKDIR $SRC/zstandard
COPY build.sh $SRC/
