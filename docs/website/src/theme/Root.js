import React, {useEffect} from 'react';
import useBaseUrl from '@docusaurus/useBaseUrl';

export default function Root({children}) {
  const panorama = useBaseUrl('/img/diagrams/persisting/zodiac-figures.png');

  useEffect(() => {
    let frame = 0;
    const updateScrollProgress = () => {
      frame = 0;
      const maxScroll = Math.max(1, document.documentElement.scrollHeight - window.innerHeight);
      const progress = Math.min(1, Math.max(0, window.scrollY / maxScroll));
      document.documentElement.style.setProperty('--scroll-progress', progress.toFixed(4));
      document.documentElement.style.setProperty('--scroll-y', `${Math.round(window.scrollY)}px`);
    };
    const onScroll = () => {
      if (!frame) frame = window.requestAnimationFrame(updateScrollProgress);
    };
    updateScrollProgress();
    window.addEventListener('scroll', onScroll, {passive: true});
    window.addEventListener('resize', onScroll, {passive: true});
    return () => {
      window.removeEventListener('scroll', onScroll);
      window.removeEventListener('resize', onScroll);
      if (frame) window.cancelAnimationFrame(frame);
    };
  }, []);

  return <>
    <div className="site-sky-loop" aria-hidden="true">
      <img src={panorama} alt="" /><img src={panorama} alt="" />
    </div>
    {children}
  </>;
}
