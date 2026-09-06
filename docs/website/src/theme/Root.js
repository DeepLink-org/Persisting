import React, {useEffect} from 'react';
import useBaseUrl from '@docusaurus/useBaseUrl';

export default function Root({children}) {
  const panorama = useBaseUrl('/img/diagrams/persisting/zodiac-figures.png');

  useEffect(() => {
    let frame = 0;
    let zodiacTimer;
    const northernHemisphereSigns = [
      'capricorn', 'aquarius', 'pisces', 'aries', 'taurus', 'gemini',
      'cancer', 'leo', 'virgo', 'libra', 'scorpio', 'sagittarius',
    ];
    const updateZodiac = () => {
      const now = new Date();
      const month = now.getMonth();
      const nextMonth = new Date(now.getFullYear(), month + 1, 1);
      const monthStart = new Date(now.getFullYear(), month, 1);
      const monthProgress = (now - monthStart) / (nextMonth - monthStart);
      const signIndex = (month + 9) % 12;
      document.documentElement.style.setProperty(
        '--zodiac-index',
        (signIndex + monthProgress).toFixed(3),
      );
      document.documentElement.dataset.zodiacSign = northernHemisphereSigns[signIndex];
    };
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
    updateZodiac();
    zodiacTimer = window.setInterval(updateZodiac, 60 * 60 * 1000);
    window.addEventListener('scroll', onScroll, {passive: true});
    window.addEventListener('resize', onScroll, {passive: true});
    return () => {
      window.removeEventListener('scroll', onScroll);
      window.removeEventListener('resize', onScroll);
      if (frame) window.cancelAnimationFrame(frame);
      if (zodiacTimer) window.clearInterval(zodiacTimer);
    };
  }, []);

  return <>
    <div className="site-sky-loop" aria-hidden="true">
      <img src={panorama} alt="" /><img src={panorama} alt="" />
    </div>
    {children}
  </>;
}
